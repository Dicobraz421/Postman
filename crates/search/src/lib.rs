//! Поиск по письмам на tantivy: словоформы (Snowball, русский + английский),
//! опечатки (расстояние Левенштейна 1–2, перестановки — за одну правку),
//! поиск по мере набора (префикс последнего слова), неверная раскладка
//! («jnxtn» → «отчет»), ё = е. Все слова запроса обязательны, каждое — в любом виде.

use std::path::Path;
use std::sync::Mutex;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{DocId, Index, IndexReader, IndexWriter, ReloadPolicy, Score, SegmentReader, TantivyDocument, Term, doc};

pub use tantivy::TantivyError as Error;
pub type Result<T> = tantivy::Result<T>;

const STEM: &str = "pm_stem";
const RAW: &str = "pm_raw";
/// Память индексатора: больше не нужно для почтового ящика.
const WRITER_HEAP: usize = 15 * 1024 * 1024;

/// Что индексируется о письме.
pub struct Doc<'a> {
    pub id: &'a str,
    pub subject: &'a str,
    /// Имя и адрес отправителя/получателей.
    pub people: &'a str,
    /// Превью или полный текст тела, если он уже известен.
    pub body: &'a str,
    pub attachments: &'a str,
    /// Unix-время получения: при равной релевантности свежие письма выше.
    pub received_at: i64,
}

struct Fields {
    id: Field,
    subject: Field,
    people: Field,
    body: Field,
    /// Всё вместе без стемминга — для опечаток и префиксов.
    raw: Field,
    received: Field,
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
            let raw = normalize(&format!("{} {} {} {}", d.subject, d.people, d.body, d.attachments));
            w.add_document(doc!(
                self.f.id => d.id,
                self.f.subject => normalize(d.subject),
                self.f.people => normalize(d.people),
                self.f.body => normalize(&format!("{} {}", d.body, d.attachments)),
                self.f.raw => raw,
                self.f.received => d.received_at,
            ))?;
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

    /// Зафиксировать изменения и освободить писателя (вместе с его памятью).
    pub fn commit(&self) -> Result<()> {
        let Some(mut w) = self.writer().take() else { return Ok(()) };
        w.commit()?;
        // Дождаться фоновых слияний сегментов, чтобы не оставлять потоков.
        w.wait_merging_threads()?;
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
        let words: Vec<String> =
            normalize(query).split(|c: char| !c.is_alphanumeric() && !"[];',.`".contains(c)).filter(|w| !w.is_empty()).map(str::to_string).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let last = words.len() - 1;
        let clauses: Vec<(Occur, Box<dyn Query>)> = words.iter().enumerate().map(|(i, w)| (Occur::Must, self.word_query(w, i == last))).collect();
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
        let top = searcher.search(&BooleanQuery::new(clauses), &collector)?;
        top.into_iter()
            .map(|(_, addr)| {
                let d: TantivyDocument = searcher.doc(addr)?;
                Ok(d.get_first(self.f.id).and_then(|v| v.as_str()).unwrap_or_default().to_string())
            })
            .collect()
    }

    /// Любой вид слова: словоформа, опечатка, начало слова, другая раскладка.
    fn word_query(&self, word: &str, is_last: bool) -> Box<dyn Query> {
        let mut any: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (w, penalty) in std::iter::once((word.to_string(), 1.0)).chain(switch_layout(word).map(|w| (w, 0.8))) {
            any.extend(self.variants(&w, is_last, penalty));
        }
        Box::new(BooleanQuery::new(any))
    }

    fn variants(&self, word: &str, is_last: bool, penalty: f32) -> Vec<(Occur, Box<dyn Query>)> {
        let mut out: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let boost = |q: Box<dyn Query>, b: f32| -> (Occur, Box<dyn Query>) { (Occur::Should, Box::new(BoostQuery::new(q, b * penalty))) };
        // Точная словоформа через стеммер: сильнее всего, тема важнее тела.
        for (field, b) in [(self.f.subject, 3.0), (self.f.people, 2.5), (self.f.body, 1.5)] {
            for t in self.stem_terms(field, word) {
                out.push(boost(Box::new(TermQuery::new(t, IndexRecordOption::WithFreqs)), b));
            }
        }
        let chars = word.chars().count();
        let raw = Term::from_field_text(self.f.raw, word);
        // Опечатки: короткие слова — только точно, длинные — до двух правок.
        let distance = match chars {
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
        };
        if distance > 0 {
            out.push(boost(Box::new(FuzzyTermQuery::new(raw.clone(), distance, true)), 0.6));
        }
        // Последнее слово — ещё набирается: ищем по началу.
        if is_last && chars >= 2 {
            out.push(boost(Box::new(FuzzyTermQuery::new_prefix(raw, if chars >= 5 { 1 } else { 0 }, true)), 0.8));
        }
        out
    }

    fn stem_terms(&self, field: Field, word: &str) -> Vec<Term> {
        // Анализатор регистрируется при открытии индекса; если его нет — ищем без словоформ.
        let Some(mut analyzer) = self.index.tokenizers().get(STEM) else { return Vec::new() };
        let mut stream = analyzer.token_stream(word);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(Term::from_field_text(field, &stream.token().text));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> SearchIndex {
        let i = SearchIndex::in_memory().unwrap();
        let d = |id, subject, people, body| Doc { id, subject, people, body, attachments: "", received_at: 0 };
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
        let d = Doc { id: "5", subject: "Отпуск", people: "Кадры", body: "Заявление", attachments: "", received_at: 0 };
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
        let d = |id, received_at| Doc { id, subject: "Pipeline failed", people: "", body: "", attachments: "", received_at };
        i.upsert(&[d("old", 1_000), d("new", 2_000_000_000), d("mid", 1_700_000_000)]).unwrap();
        i.commit().unwrap();
        assert_eq!(i.search("pipelin faild", 10).unwrap(), ["new", "mid", "old"]);
    }

    #[test]
    fn replace_and_delete() {
        let i = index();
        i.upsert(&[Doc { id: "2", subject: "Обед", people: "", body: "", attachments: "", received_at: 0 }]).unwrap();
        i.delete(&["1".into()]).unwrap();
        i.commit().unwrap();
        assert!(i.search("совещание", 10).unwrap().is_empty());
        assert!(i.search("отчет", 10).unwrap().is_empty());
        assert_eq!(i.len(), 3);
    }
}
