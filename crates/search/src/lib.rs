//! Поиск по письмам на tantivy: словоформы (Snowball, русский + английский),
//! опечатки (расстояние Левенштейна 1–2, перестановки — за одну правку),
//! поиск по мере набора (префикс последнего слова), неверная раскладка
//! («jnxtn» → «отчет»), ё = е. Все слова запроса обязательны, каждое — в любом виде.
//!
//! Вложения (T3-10): имя и текст каждого хранятся в документе письма, ответ ([`SearchIndex::hits`]) называет
//! вложение, в котором нашлись слова запроса, и отрывок вокруг совпадения. Текст вложения индексу дают готовым
//! (его достаёт воркер-песочница), здесь он только режется на слова.
//!
//! Один индекс на почту и все каналы бесед (ADR 0012 п. 10): у документа — канал ([`Doc::channel`], `mail` у писем),
//! адрес беседы ([`Doc::conv`]) и адреса автора ([`Doc::authors`], строчными) — точные поля для фильтров
//! ([`Scope`], [`SearchIndex::hits_in`]).

use std::collections::BTreeMap;

use std::path::Path;
use std::sync::Mutex;

use tantivy::collector::{DocSetCollector, TopDocs};
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{DocId, Index, IndexReader, IndexWriter, ReloadPolicy, Score, SegmentReader, TantivyDocument, Term, doc};

pub use tantivy::TantivyError as Error;
pub type Result<T> = tantivy::Result<T>;

const STEM: &str = "pm_stem";
const RAW: &str = "pm_raw";
/// Память индексатора: больше не нужно для почтового ящика.
const WRITER_HEAP: usize = 15 * 1024 * 1024;
/// Текст одного вложения в индексе — столько байт от начала; всех вложений письма вместе — [`LETTER_TEXT`].
pub const ATTACHMENT_TEXT: usize = 64 * 1024;
pub const LETTER_TEXT: usize = 256 * 1024;
/// Отрывок вокруг совпадения во вложении, байт (кириллица — вдвое меньше знаков).
const SNIPPET: usize = 160;

/// Вложение письма: имя и текст, если он известен (пусто — ищется только имя).
#[derive(Debug, Clone, Copy, Default)]
pub struct Attachment<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// Найденное письмо.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: String,
    /// Слова запроса нашлись во вложении (в имени или тексте), а само письмо (тема, люди, тело) запросу не отвечает:
    /// без вложения его бы не нашли. Письмо отвечает запросу и само — `None` (решение владельца 2026-11-11).
    pub attachment: Option<AttachmentHit>,
}

/// Где совпало во вложении: имя и отрывок текста вокруг совпадения («…» — отрывок не с начала или не до конца);
/// совпало только имя или текста нет — отрывок пуст.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttachmentHit {
    pub name: String,
    pub snippet: String,
}

/// Что индексируется о письме или сообщении канала.
#[derive(Default)]
pub struct Doc<'a> {
    pub id: &'a str,
    pub subject: &'a str,
    /// Имя и адрес отправителя/получателей.
    pub people: &'a str,
    /// Превью или полный текст тела, если он уже известен.
    pub body: &'a str,
    pub attachments: &'a [Attachment<'a>],
    /// Unix-время получения: при равной релевантности свежие письма выше.
    pub received_at: i64,
    /// Канал: `mail` у писем, `telegram`, `matrix`, `demo` у сообщений; пусто — без канала (находится только без
    /// фильтра канала).
    pub channel: &'a str,
    /// Адрес беседы `postman://…` (у писем — `postman://mail/<ящик>/<цепочка>`; ящик ещё неизвестен — пусто).
    pub conv: &'a str,
    /// Адреса автора: участник канала, его почта и телефон; у письма — адрес «От». В индексе — строчными.
    pub authors: &'a [String],
}

/// Фильтр выдачи (ADR 0012 п. 10): канал, беседа, автор — точные значения; пустой список — без условия, несколько
/// значений — любое из них.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub channels: Vec<String>,
    pub convs: Vec<String>,
    /// Адреса автора (`от:` — все адреса одного человека); регистр не важен.
    pub authors: Vec<String>,
}

impl Scope {
    /// Только канал `channel`.
    pub fn channel(channel: &str) -> Self {
        Self { channels: vec![channel.to_owned()], ..Self::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.convs.is_empty() && self.authors.is_empty()
    }
}

struct Fields {
    id: Field,
    subject: Field,
    people: Field,
    body: Field,
    /// Всё вместе без стемминга — для опечаток и префиксов.
    raw: Field,
    received: Field,
    /// Имена и тексты вложений вместе: словоформы и (без стемминга) опечатки с префиксами — отдельно от письма,
    /// чтобы знать, отвечает ли письмо запросу и без вложений.
    att: Field,
    att_raw: Field,
    /// Имена и тексты вложений по порядку, только хранятся: из них — имя и отрывок в ответе.
    att_name: Field,
    att_text: Field,
    /// Канал, беседа, автор — точные значения для фильтров ([`Scope`]).
    channel: Field,
    conv: Field,
    author: Field,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    /// Писатель живёт только между записью и коммитом: в покое память не держит.
    writer: Mutex<Option<IndexWriter>>,
    f: Fields,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text = |tok: &str| {
        TextOptions::default().set_indexing_options(TextFieldIndexing::default().set_tokenizer(tok).set_index_option(IndexRecordOption::WithFreqs))
    };
    let f = Fields {
        id: b.add_text_field("id", STRING | STORED),
        subject: b.add_text_field("subject", text(STEM)),
        people: b.add_text_field("people", text(STEM)),
        body: b.add_text_field("body", text(STEM)),
        raw: b.add_text_field("raw", text(RAW)),
        received: b.add_i64_field("received", FAST),
        att: b.add_text_field("att", text(STEM)),
        att_raw: b.add_text_field("att_raw", text(RAW)),
        att_name: b.add_text_field("att_name", STORED),
        att_text: b.add_text_field("att_text", STORED),
        channel: b.add_text_field("channel", STRING),
        conv: b.add_text_field("conv", STRING),
        author: b.add_text_field("author", STRING),
    };
    (b.build(), f)
}

fn register(index: &Index) {
    let stem = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(Stemmer::new(Language::Russian))
        .filter(Stemmer::new(Language::English))
        .build();
    let raw = TextAnalyzer::builder(SimpleTokenizer::default()).filter(RemoveLongFilter::limit(40)).filter(LowerCaser).build();
    index.tokenizers().register(STEM, stem);
    index.tokenizers().register(RAW, raw);
}

/// ё → е и нижний регистр: так и индексируем, и ищем.
fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ё' | 'Ё' => 'е',
            c => c,
        })
        .collect::<String>()
        .to_lowercase()
}

/// ё → е без смены длины в байтах: позиции слов — те же, что в исходном тексте.
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ё' => 'е',
            'Ё' => 'Е',
            c => c,
        })
        .collect()
}

/// Начало текста не длиннее `max` байт, по границе знака.
fn head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Слова запроса: как в индексе, плюс знаки клавиш с буквами другой раскладки.
fn words(query: &str) -> Vec<String> {
    normalize(query).split(|c: char| !c.is_alphanumeric() && !"[];',.`".contains(c)).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Сколько правок прощается слову такой длины: короткие — только точно, длинные — до двух.
fn typos(chars: usize) -> usize {
    match chars {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

/// Слова отличаются не больше чем на `max` правок (замена, вставка, пропуск, перестановка соседних).
fn within(a: &[char], b: &[char], max: usize) -> bool {
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    let (mut before, mut prev): (Vec<usize>, Vec<usize>) = (Vec::new(), (0..=b.len()).collect());
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (prev[j] + 1).min(row[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                row[j] = row[j].min(before[j - 2] + 1);
            }
        }
        before = std::mem::replace(&mut prev, row);
    }
    prev[b.len()] <= max
}

/// Слово запроса во всех видах, в каких его ищет индекс ([`SearchIndex::variants`]), — для сверки со словами вложения.
struct Wanted {
    /// Как набрано и в другой раскладке.
    forms: Vec<Vec<char>>,
    stems: Vec<String>,
    typos: usize,
    /// Последнее слово ещё набирается: годится и начало слова.
    prefix: bool,
}

impl Wanted {
    fn matches(&self, raw: &str, stem: &str) -> bool {
        if self.stems.iter().any(|s| s == stem) {
            return true;
        }
        let token: Vec<char> = raw.chars().collect();
        self.forms.iter().any(|f| {
            (self.typos > 0 && within(f, &token, self.typos))
                || (self.prefix && f.len() >= 2 && {
                    let exact = token.starts_with(f);
                    // Начало с одной опечаткой — у слов от пяти букв, как в запросе к индексу.
                    exact || (f.len() >= 5 && (f.len() - 1..=f.len() + 1).any(|n| token.len() >= n && within(f, &token[..n], 1)))
                })
        })
    }
}

const QWERTY: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
const JCUKEN: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";

/// Слово, набранное в другой раскладке: «jnxtn» → «отчет», «руддщ» → «hello».
fn switch_layout(word: &str) -> Option<String> {
    let map = |from: &str, to: &str| -> Option<String> {
        word.chars().map(|c| from.chars().position(|f| f == c).and_then(|i| to.chars().nth(i))).collect()
    };
    if word.chars().all(|c| QWERTY.contains(c)) {
        map(QWERTY, JCUKEN)
    } else if word.chars().all(|c| JCUKEN.contains(c)) {
        map(JCUKEN, QWERTY)
    } else {
        None
    }
}

impl SearchIndex {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (schema, f) = schema();
        let index = match Index::open_in_dir(dir) {
            Ok(i) if i.schema() == schema => i,
            // Нет индекса или старая схема: индекс — производные данные, пересоздаём.
            _ => {
                for e in std::fs::read_dir(dir)?.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
                Index::create_in_dir(dir, schema)?
            }
        };
        Self::with(index, f)
    }

    pub fn in_memory() -> Result<Self> {
        let (schema, f) = schema();
        Self::with(Index::create_in_ram(schema), f)
    }

    fn with(index: Index, f: Fields) -> Result<Self> {
        register(&index);
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        Ok(Self { index, reader, writer: Mutex::new(None), f })
    }

    /// Писатель индекса. Мьютекс, отравленный паникой в другом потоке, не роняет поиск: писатель
    /// tantivy сам по себе цел, незакоммиченное просто повторится.
    fn writer(&self) -> std::sync::MutexGuard<'_, Option<IndexWriter>> {
        self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn with_writer<R>(&self, f: impl FnOnce(&IndexWriter) -> Result<R>) -> Result<R> {
        let mut guard = self.writer();
        let w = match guard.as_mut() {
            Some(w) => w,
            // Один поток индексации с минимальным бюджетом: письма приходят порциями, память важнее.
            None => guard.insert(self.index.writer_with_num_threads(1, WRITER_HEAP)?),
        };
        f(w)
    }

    /// Есть ли незакоммиченные изменения.
    pub fn dirty(&self) -> bool {
        self.writer().is_some()
    }

    /// Добавить или заменить письма. Видны в поиске после [`commit`](Self::commit).
    pub fn upsert(&self, docs: &[Doc<'_>]) -> Result<()> {
        self.with_writer(|w| self.write(w, docs))
    }

    fn write(&self, w: &IndexWriter, docs: &[Doc<'_>]) -> Result<()> {
        for d in docs {
            w.delete_term(Term::from_field_text(self.f.id, d.id));
            // Вложения: в индексе — столько текста, сколько хранится, иначе письмо нашлось бы без отрывка.
            let (mut attached, mut left) = (String::new(), LETTER_TEXT);
            let mut stored: Vec<(&str, &str)> = Vec::with_capacity(d.attachments.len());
            for a in d.attachments {
                let text = head(a.text, ATTACHMENT_TEXT.min(left));
                left -= text.len();
                for part in [a.name, text] {
                    attached.push(' ');
                    attached.push_str(part);
                }
                stored.push((a.name, text));
            }
            let attached = normalize(&attached);
            let mut document = doc!(
                self.f.id => d.id,
                self.f.subject => normalize(d.subject),
                self.f.people => normalize(d.people),
                self.f.body => normalize(d.body),
                self.f.raw => normalize(&format!("{} {} {}", d.subject, d.people, d.body)),
                self.f.att => attached.clone(),
                self.f.att_raw => attached,
                self.f.received => d.received_at,
            );
            for (name, text) in stored {
                document.add_text(self.f.att_name, name);
                document.add_text(self.f.att_text, text);
            }
            for (field, value) in [(self.f.channel, d.channel), (self.f.conv, d.conv)] {
                if !value.is_empty() {
                    document.add_text(field, value);
                }
            }
            for a in d.authors.iter().filter(|a| !a.is_empty()) {
                document.add_text(self.f.author, a.to_lowercase());
            }
            w.add_document(document)?;
        }
        Ok(())
    }

    pub fn delete(&self, ids: &[String]) -> Result<()> {
        self.with_writer(|w| {
            for id in ids {
                w.delete_term(Term::from_field_text(self.f.id, id));
            }
            Ok(())
        })
    }

    /// Убрать все сообщения беседы `conv` (адрес `postman://…`): беседы больше нет. Видно после [`commit`](Self::commit).
    pub fn delete_conversation(&self, conv: &str) -> Result<()> {
        self.with_writer(|w| {
            w.delete_term(Term::from_field_text(self.f.conv, conv));
            Ok(())
        })
    }

    /// Убрать все документы канала `channel` (пересборка сообщений каналов). Видно после [`commit`](Self::commit).
    pub fn delete_channel(&self, channel: &str) -> Result<()> {
        self.with_writer(|w| {
            w.delete_term(Term::from_field_text(self.f.channel, channel));
            Ok(())
        })
    }

    /// Убрать все письма. Видно после [`commit`](Self::commit).
    pub fn clear(&self) -> Result<()> {
        self.with_writer(|w| w.delete_all_documents().map(|_| ()))
    }

    /// Письмо записано в индекс (после [`commit`](Self::commit)). Удалённое может числиться до слияния сегментов.
    pub fn contains(&self, id: &str) -> Result<bool> {
        Ok(self.reader.searcher().doc_freq(&Term::from_field_text(self.f.id, id))? > 0)
    }

    /// Зафиксировать изменения и освободить писателя (вместе с его памятью). Замок — до конца: второй писатель
    /// на занятом индексе не откроется, и письмо из другого потока потерялось бы.
    pub fn commit(&self) -> Result<()> {
        let mut guard = self.writer();
        let Some(mut w) = guard.take() else { return Ok(()) };
        w.commit()?;
        // Дождаться фоновых слияний сегментов, чтобы не оставлять потоков.
        w.wait_merging_threads()?;
        drop(guard);
        self.reader.reload()
    }

    pub fn len(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// id писем по убыванию релевантности.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        Ok(self.hits(query, limit)?.into_iter().map(|h| h.id).collect())
    }

    /// Письма по убыванию релевантности — и вложение, в котором совпало ([`Hit::attachment`]).
    pub fn hits(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        self.hits_in(query, &Scope::default(), limit)
    }

    /// Условия фильтра: каждое заданное поле — одно из значений.
    fn scope_query(&self, scope: &Scope) -> Vec<(Occur, Box<dyn Query>)> {
        let any = |field: Field, values: &[String], lower: bool| -> Option<(Occur, Box<dyn Query>)> {
            if values.is_empty() {
                return None;
            }
            let terms = values
                .iter()
                .map(|v| {
                    let v = if lower { v.to_lowercase() } else { v.clone() };
                    let q: Box<dyn Query> = Box::new(TermQuery::new(Term::from_field_text(field, &v), IndexRecordOption::Basic));
                    (Occur::Should, q)
                })
                .collect();
            let q: Box<dyn Query> = Box::new(BooleanQuery::new(terms));
            Some((Occur::Must, q))
        };
        [any(self.f.channel, &scope.channels, false), any(self.f.conv, &scope.convs, false), any(self.f.author, &scope.authors, true)]
            .into_iter()
            .flatten()
            .collect()
    }

    /// [`hits`](Self::hits) в пределах фильтра `scope` (канал, беседа, автор). Слов нет, а фильтр есть — всё, что
    /// ему отвечает, новые сверху.
    pub fn hits_in(&self, query: &str, scope: &Scope, limit: usize) -> Result<Vec<Hit>> {
        let words = words(query);
        if words.is_empty() {
            let filter = self.scope_query(scope);
            return if filter.is_empty() { Ok(Vec::new()) } else { self.newest(filter, limit) };
        }
        let last = words.len() - 1;
        let query = |own: bool| -> BooleanQuery {
            let mut all: Vec<(Occur, Box<dyn Query>)> =
                words.iter().enumerate().map(|(i, w)| (Occur::Must, self.word_query(w, i == last, own))).collect();
            all.extend(self.scope_query(scope));
            BooleanQuery::new(all)
        };
        let searcher = self.reader.searcher();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let collector = TopDocs::with_limit(limit).tweak_score(move |seg: &SegmentReader| {
            let received = seg.fast_fields().i64("received").ok();
            move |doc: DocId, score: Score| {
                let at = received.as_ref().and_then(|c| c.first(doc)).unwrap_or(0);
                let age_days = ((now - at).max(0) as f32) / 86_400.0;
                // До +50% к релевантности для свежих писем (затухание ~2 месяца)
                // и слабая поправка на возраст, чтобы и среди старых новые были выше.
                score * (1.0 + 0.5 * (-age_days / 60.0).exp() + 0.01 / (1.0 + age_days / 365.0))
            }
        });
        let top = searcher.search(&query(false), &collector)?;
        let wanted: Vec<Wanted> = words.iter().enumerate().map(|(i, w)| self.wanted(w, i == last)).collect();
        let mut hits = Vec::with_capacity(top.len());
        for (_, addr) in &top {
            let d: TantivyDocument = searcher.doc(*addr)?;
            let id = d.get_first(self.f.id).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let names = d.get_all(self.f.att_name).filter_map(|v| v.as_str());
            let texts = d.get_all(self.f.att_text).filter_map(|v| v.as_str());
            hits.push(Hit { id, attachment: self.attachment_hit(&wanted, names.zip(texts)) });
        }
        // Письмо отвечает запросу и без вложений — вложение не называем: один запрос на всех, только если есть кого проверять.
        if hits.iter().any(|h| h.attachment.is_some()) {
            let own = searcher.search(&query(true), &DocSetCollector)?;
            for (hit, (_, addr)) in hits.iter_mut().zip(&top) {
                if own.contains(addr) {
                    hit.attachment = None;
                }
            }
        }
        Ok(hits)
    }

    /// Документы фильтра без слов: новые сверху.
    fn newest(&self, filter: Vec<(Occur, Box<dyn Query>)>, limit: usize) -> Result<Vec<Hit>> {
        let mut all = filter;
        all.push((Occur::Must, Box::new(AllQuery)));
        let searcher = self.reader.searcher();
        let collector = TopDocs::with_limit(limit).order_by_fast_field::<i64>("received", tantivy::Order::Desc);
        let top = searcher.search(&BooleanQuery::new(all), &collector)?;
        let mut out = Vec::with_capacity(top.len());
        for (_, addr) in top {
            let d: TantivyDocument = searcher.doc(addr)?;
            out.push(Hit { id: d.get_first(self.f.id).and_then(|v| v.as_str()).unwrap_or_default().to_owned(), attachment: None });
        }
        Ok(out)
    }

    fn wanted(&self, word: &str, is_last: bool) -> Wanted {
        let forms: Vec<String> = std::iter::once(word.to_string()).chain(switch_layout(word)).collect();
        Wanted {
            stems: forms.iter().flat_map(|f| self.stems(f)).collect(),
            typos: typos(word.chars().count()),
            prefix: is_last,
            forms: forms.iter().map(|f| f.chars().collect()).collect(),
        }
    }

    /// Вложение, в котором нашлось больше всего слов запроса (при равенстве — первое), и отрывок его текста.
    fn attachment_hit<'d>(&self, wanted: &[Wanted], attachments: impl Iterator<Item = (&'d str, &'d str)>) -> Option<AttachmentHit> {
        let mut best: Option<(usize, AttachmentHit)> = None;
        for (name, text) in attachments {
            let mut found = vec![false; wanted.len()];
            self.scan(&fold(name), wanted, &mut found);
            let folded = fold(text);
            let terms = self.scan(&folded, wanted, &mut found);
            let n = found.iter().filter(|f| **f).count();
            if n > best.as_ref().map_or(0, |b| b.0) {
                best = Some((n, AttachmentHit { name: name.to_owned(), snippet: self.snippet(text, &folded, terms) }));
            }
        }
        best.map(|b| b.1)
    }

    /// Слова текста (после [`fold`]), совпавшие со словами запроса, — как их видит анализатор без стемминга; `found` —
    /// какие слова запроса встретились.
    fn scan(&self, folded: &str, wanted: &[Wanted], found: &mut [bool]) -> BTreeMap<String, Score> {
        let mut terms = BTreeMap::new();
        let tokenizers = self.index.tokenizers();
        let (Some(mut raw), Some(mut stem)) = (tokenizers.get(RAW), tokenizers.get(STEM)) else { return terms };
        // Оба анализатора режут текст одинаково: слово и его основа идут парой.
        let (mut raw, mut stem) = (raw.token_stream(folded), stem.token_stream(folded));
        while raw.advance() && stem.advance() {
            let (token, base) = (&raw.token().text, &stem.token().text);
            if terms.contains_key(token) {
                continue;
            }
            let mut hit = false;
            for (w, f) in wanted.iter().zip(found.iter_mut()) {
                if w.matches(token, base) {
                    (*f, hit) = (true, true);
                }
            }
            if hit {
                terms.insert(token.clone(), 1.0);
            }
        }
        terms
    }

    /// Отрывок текста вложения вокруг совпавших слов — сниппет tantivy; `folded` — тот же текст для анализатора.
    fn snippet(&self, text: &str, folded: &str, terms: BTreeMap<String, Score>) -> String {
        let Some(raw) = self.index.tokenizers().get(RAW).filter(|_| !terms.is_empty()) else { return String::new() };
        let snippet = SnippetGenerator::new(terms, raw, self.f.att_text, SNIPPET).snippet(folded);
        let range = snippet.fragment_range();
        // Позиции в `folded` и в исходном тексте совпадают ([`fold`]); не сошлось — отрывок без «ё».
        let Some(fragment) = text.get(range.clone()) else { return snippet.fragment().split_whitespace().collect::<Vec<_>>().join(" ") };
        // Отрывок кончается на слове: знаки до края текста — его часть, слова за краем — «…».
        let (before, after) = (&text[..range.start], &text[range.end..]);
        let wordless = |s: &str| !s.chars().any(char::is_alphanumeric);
        let (from, to) = (if wordless(before) { 0 } else { range.start }, if wordless(after) { text.len() } else { range.end });
        let mut out: String = text[from..to].split_whitespace().collect::<Vec<_>>().join(" ");
        if fragment.is_empty() || out.is_empty() {
            return String::new();
        }
        if from > 0 {
            out.insert(0, '…');
        }
        if to < text.len() {
            out.push('…');
        }
        out
    }

    /// Любой вид слова: словоформа, опечатка, начало слова, другая раскладка. `own` — только в самом письме, без
    /// вложений.
    fn word_query(&self, word: &str, is_last: bool, own: bool) -> Box<dyn Query> {
        let mut any: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (w, penalty) in std::iter::once((word.to_string(), 1.0)).chain(switch_layout(word).map(|w| (w, 0.8))) {
            any.extend(self.variants(&w, is_last, penalty, own));
        }
        Box::new(BooleanQuery::new(any))
    }

    fn variants(&self, word: &str, is_last: bool, penalty: f32, own: bool) -> Vec<(Occur, Box<dyn Query>)> {
        let mut out: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let boost = |q: Box<dyn Query>, b: f32| -> (Occur, Box<dyn Query>) { (Occur::Should, Box::new(BoostQuery::new(q, b * penalty))) };
        // Точная словоформа через стеммер: сильнее всего, тема важнее тела; вложения — наравне с телом.
        let stemmed = [(self.f.subject, 3.0), (self.f.people, 2.5), (self.f.body, 1.5), (self.f.att, 1.5)];
        for (field, b) in stemmed.into_iter().take(if own { 3 } else { 4 }) {
            for t in self.stem_terms(field, word) {
                out.push(boost(Box::new(TermQuery::new(t, IndexRecordOption::WithFreqs)), b));
            }
        }
        let chars = word.chars().count();
        // Опечатки: короткие слова — только точно, длинные — до двух правок.
        let distance = typos(chars) as u8;
        for field in [self.f.raw, self.f.att_raw].into_iter().take(if own { 1 } else { 2 }) {
            let raw = Term::from_field_text(field, word);
            if distance > 0 {
                out.push(boost(Box::new(FuzzyTermQuery::new(raw.clone(), distance, true)), 0.6));
            }
            // Последнее слово — ещё набирается: ищем по началу.
            if is_last && chars >= 2 {
                out.push(boost(Box::new(FuzzyTermQuery::new_prefix(raw, if chars >= 5 { 1 } else { 0 }, true)), 0.8));
            }
        }
        out
    }

    fn stem_terms(&self, field: Field, word: &str) -> Vec<Term> {
        self.stems(word).iter().map(|s| Term::from_field_text(field, s)).collect()
    }

    /// Основы слова.
    fn stems(&self, word: &str) -> Vec<String> {
        // Анализатор регистрируется при открытии индекса; если его нет — ищем без словоформ.
        let Some(mut analyzer) = self.index.tokenizers().get(STEM) else { return Vec::new() };
        let mut stream = analyzer.token_stream(word);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> SearchIndex {
        let i = SearchIndex::in_memory().unwrap();
        let d = |id, subject, people, body| Doc { id, subject, people, body, attachments: &[], received_at: 0, ..Doc::default() };
        i.upsert(&[
            d("1", "Отчёт за третий квартал", "Иванова Мария", "Направляю отчёты по продажам"),
            d("2", "Совещание по проекту", "Петров Алексей", "Перенесём встречу на пятницу"),
            d("3", "Pipeline failed", "GitLab", "Job deploy failed on main"),
            d("4", "Счета на оплату", "Бухгалтерия", "Прошу оплатить счёт до конца месяца"),
        ])
        .unwrap();
        i.commit().unwrap();
        i
    }

    /// S2-03: паника в потоке, державшем писателя, не роняет индекс — запись и поиск продолжают работать.
    #[test]
    fn poisoned_writer_lock_does_not_break_the_index() {
        let i = std::sync::Arc::new(index());
        let held = i.clone();
        let _ = std::thread::spawn(move || {
            let _guard = held.writer.lock();
            panic!("паника с захваченным писателем");
        })
        .join();
        assert!(i.writer.is_poisoned());
        assert!(!i.dirty());
        let d = Doc {
            id: "5", subject: "Отпуск", people: "Кадры", body: "Заявление", attachments: &[], received_at: 0, ..Doc::default()
        };
        i.upsert(&[d]).unwrap();
        assert!(i.dirty());
        i.commit().unwrap();
        assert_eq!(i.search("отпуск", 10).unwrap(), ["5"]);
    }

    #[test]
    fn word_forms() {
        let i = index();
        assert_eq!(i.search("отчеты", 10).unwrap(), ["1"]);
        assert_eq!(i.search("совещания проекта", 10).unwrap(), ["2"]);
        assert_eq!(i.search("счет", 10).unwrap(), ["4"]);
        assert_eq!(i.search("deploying", 10).unwrap(), ["3"]);
    }

    #[test]
    fn typos_prefix_layout() {
        let i = index();
        assert_eq!(i.search("кваратл", 10).unwrap(), ["1"]);
        assert_eq!(i.search("иваноав", 10).unwrap(), ["1"]);
        assert_eq!(i.search("бухгал", 10).unwrap(), ["4"]);
        assert_eq!(i.search("jnxtn", 10).unwrap(), ["1"]);
        assert_eq!(i.search("зшзудшту", 10).unwrap(), ["3"]);
        assert!(i.search("несуществующее слово", 10).unwrap().is_empty());
    }

    #[test]
    fn newer_first_on_equal_relevance() {
        let i = SearchIndex::in_memory().unwrap();
        let d = |id, received_at| Doc { id, subject: "Pipeline failed", people: "", body: "", attachments: &[], received_at, ..Doc::default() };
        i.upsert(&[d("old", 1_000), d("new", 2_000_000_000), d("mid", 1_700_000_000)]).unwrap();
        i.commit().unwrap();
        assert_eq!(i.search("pipelin faild", 10).unwrap(), ["new", "mid", "old"]);
    }

    /// Запись из другого потока во время коммита ждёт его: письма не теряются.
    #[test]
    fn writes_during_commit_are_kept() {
        let i = std::sync::Arc::new(SearchIndex::in_memory().unwrap());
        let writers: Vec<_> = (0..2)
            .map(|n| {
                let i = i.clone();
                std::thread::spawn(move || {
                    for k in 0..20 {
                        let id = format!("{n}-{k}");
                        i.upsert(&[Doc {
                            id: &id, subject: "Письмо", people: "", body: "", attachments: &[], received_at: 0, ..Doc::default()
                        }])
                        .unwrap();
                        i.commit().unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        i.commit().unwrap();
        assert_eq!(i.len(), 40);
    }

    /// Индекс помнит письма по id; очищенный пуст, в него пишут дальше.
    #[test]
    fn contains_and_clear() {
        let i = index();
        assert!(i.contains("1").unwrap() && !i.contains("#1").unwrap());
        i.clear().unwrap();
        i.commit().unwrap();
        assert!(i.is_empty() && !i.contains("1").unwrap());
        i.upsert(&[Doc { id: "#1", subject: "Обед", people: "", body: "", attachments: &[], received_at: 0, ..Doc::default() }]).unwrap();
        i.commit().unwrap();
        assert!(i.contains("#1").unwrap());
        assert_eq!(i.search("обед", 10).unwrap(), ["#1"]);
    }

    #[test]
    fn replace_and_delete() {
        let i = index();
        i.upsert(&[Doc { id: "2", subject: "Обед", people: "", body: "", attachments: &[], received_at: 0, ..Doc::default() }]).unwrap();
        i.delete(&["1".into()]).unwrap();
        i.commit().unwrap();
        assert!(i.search("совещание", 10).unwrap().is_empty());
        assert!(i.search("отчет", 10).unwrap().is_empty());
        assert_eq!(i.len(), 3);
    }
    fn hit(i: &SearchIndex, query: &str) -> Option<AttachmentHit> {
        let hits = i.hits(query, 10).unwrap();
        assert_eq!(hits.len(), 1, "{query}: {hits:?}");
        hits.into_iter().next().and_then(|h| h.attachment)
    }

    fn attached() -> SearchIndex {
        let i = SearchIndex::in_memory().unwrap();
        let long = format!("{} Ставка аренды склада — 1200 рублей за метр. {}", "Вводная часть договора. ".repeat(20), "Прочие условия. ".repeat(20));
        let d = |id, subject, body, attachments| Doc { id, subject, people: "", body, attachments, received_at: 0, ..Doc::default() };
        i.upsert(&[
            d("body", "Смета на ремонт", "Высылаю смету, посмотрите до пятницы", &[]),
            d("file", "Документы", "Во вложении", &[Attachment { name: "Договор.docx", text: &long }]),
            d(
                "both",
                "Протокол",
                "Протокол совещания по Ёлке — во вложении",
                &[Attachment { name: "logo.png", text: "" }, Attachment { name: "Протокол.txt", text: "Решили: ёлку ставим в холле." }],
            ),
            d("name", "Скан", "", &[Attachment { name: "Накладная 17.pdf", text: "" }]),
            d(
                "two",
                "Файлы",
                "",
                &[Attachment { name: "a.txt", text: "Бюджет отдела" }, Attachment { name: "b.txt", text: "Бюджет отдела на квартал" }],
            ),
        ])
        .unwrap();
        i.commit().unwrap();
        i
    }

    /// T3-10: совпало только в письме — вложение не называется.
    #[test]
    fn attachment_hit_absent_for_body_match() {
        assert_eq!(hit(&attached(), "смета"), None);
    }

    /// T3-10: совпало в тексте вложения — его имя и отрывок вокруг совпадения, а не начало текста; словоформа и
    /// опечатка находят так же, как само письмо.
    #[test]
    fn attachment_hit_names_file_and_snippet() {
        let i = attached();
        for query in ["аренды", "аренда склада", "арнеды", "fhtyls"] {
            let h = hit(&i, query).unwrap();
            assert_eq!(h.name, "Договор.docx", "{query}");
            assert!(h.snippet.contains("Ставка аренды склада"), "{query}: {}", h.snippet);
            assert!(h.snippet.starts_with('…') && h.snippet.ends_with('…'), "{query}: {}", h.snippet);
            assert!(h.snippet.chars().count() < 200, "{query}: {}", h.snippet);
        }
    }

    /// T3-10: совпало и в письме, и во вложении — письмо находится, вложение не называется (решение владельца
    /// 2026-11-11); часть слов есть только во вложении — называется; ё = е, отрывок — как в файле.
    #[test]
    fn attachment_hit_when_body_matches_too() {
        let i = attached();
        for query in ["елка", "ёлку", "протокол совещания"] {
            assert_eq!(hit(&i, query), None, "{query}");
        }
        let file = AttachmentHit { name: "Протокол.txt".into(), snippet: "Решили: ёлку ставим в холле.".into() };
        for query in ["елка в холле", "протокол ёлку ставим"] {
            assert_eq!(hit(&i, query), Some(file.clone()), "{query}");
        }
    }

    /// T3-10: совпало имя файла — вложение без отрывка; из двух вложений — то, где слов запроса больше.
    #[test]
    fn attachment_hit_by_name_and_best_of_many() {
        let i = attached();
        assert_eq!(hit(&i, "накладная"), Some(AttachmentHit { name: "Накладная 17.pdf".into(), snippet: String::new() }));
        assert_eq!(hit(&i, "бюджет квартал").map(|h| h.name).as_deref(), Some("b.txt"));
        assert_eq!(hit(&i, "бюджет").map(|h| h.name).as_deref(), Some("a.txt"));
    }

    /// T3-10: текст вложения в индексе ограничен — и ищется, и хранится одно и то же начало.
    #[test]
    fn attachment_text_is_capped() {
        let i = SearchIndex::in_memory().unwrap();
        let text = format!("{} хвостик", "слово ".repeat(ATTACHMENT_TEXT / 11 + 1));
        i.upsert(&[Doc {
            id: "1",
            subject: "",
            people: "",
            body: "",
            attachments: &[Attachment { name: "a.txt", text: &text }],
            received_at: 0,
            ..Doc::default()
        }])
        .unwrap();
        i.commit().unwrap();
        assert!(i.search("хвостик", 10).unwrap().is_empty());
        assert_eq!(hit(&i, "слово").map(|h| h.name).as_deref(), Some("a.txt"));
    }

    /// ADR 0012 п. 10: письма и сообщения каналов — в одном индексе; фильтр по каналу, беседе и автору (любой из
    /// адресов, регистр не важен); фильтр без слов — всё подходящее, новые сверху; беседа и канал удаляются целиком.
    #[test]
    fn channels_share_the_index() {
        let i = SearchIndex::in_memory().unwrap();
        let anna = ["u-anna".to_owned(), "anna@example.test".to_owned()];
        let boris = ["b.petrov@corp.example".to_owned()];
        let (team, direct) = ("postman://demo/me@example.test/g-team", "postman://demo/me@example.test/d-anna");
        let d = |id, body, channel, conv, authors, received_at| Doc {
            id,
            subject: "",
            people: "",
            body,
            channel,
            conv,
            authors,
            received_at,
            ..Doc::default()
        };
        i.upsert(&[
            d("l1", "Отчёт по сборке", "mail", "", &anna[1..], 10),
            d("c1", "Сборка готова, отчёт приложен", "demo", team, &boris, 20),
            d("c2", "Посмотри отчёт", "demo", direct, &anna, 30),
        ])
        .unwrap();
        i.commit().unwrap();
        let ids = |q: &str, s: &Scope| i.hits_in(q, s, 10).unwrap().into_iter().map(|h| h.id).collect::<Vec<_>>();
        assert_eq!(ids("отчет", &Scope::default()).len(), 3);
        assert_eq!(ids("отчет", &Scope::channel("mail")), ["l1"]);
        assert_eq!(ids("отчет", &Scope { convs: vec![team.into()], ..Scope::default() }), ["c1"]);
        let from_anna = Scope { authors: vec!["Anna@Example.test".into()], ..Scope::default() };
        assert_eq!(ids("", &from_anna), ["c2", "l1"], "без слов — всё по фильтру, новые сверху");
        assert!(ids("", &Scope::default()).is_empty());
        i.delete_conversation(team).unwrap();
        i.commit().unwrap();
        assert_eq!(ids("сборка", &Scope::default()), ["l1"]);
        i.delete_channel("demo").unwrap();
        i.commit().unwrap();
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn edits_within_distance() {
        let w = |a: &str, b: &str, max| within(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>(), max);
        assert!(w("квартал", "кваратл", 1) && w("отчет", "отчёты", 2) && !w("отчет", "отчёты", 1));
        assert!(!w("склад", "вклады", 1) && w("склад", "склад", 0));
    }
}
