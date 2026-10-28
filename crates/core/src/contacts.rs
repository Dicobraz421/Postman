//! Адресная книга для автодополнения «Кому/Копия»: недавние корреспонденты из
//! хранилища, личные контакты (JMAP ContactCard) и справочник сотрудников (Principals).
//! Ищет по началу слов имени и адреса, в любой раскладке и транслитом
//! («ivan» → «Иван», «bdfy» → «иван»); выше — частые и недавние.

use std::collections::HashMap;

use postman_cache::Cache;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Recent,
    Contact,
    Directory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub email: String,
    pub source: Source,
    /// Сколько раз встречался в переписке (письма мне и от меня).
    pub count: u32,
    /// Последний контакт, Unix-секунды.
    pub last: i64,
    /// Слова для поиска: части имени, адреса, их транслит.
    keys: Vec<String>,
}

#[derive(Default)]
pub struct AddressBook {
    entries: HashMap<String, Entry>,
}

/// Письма, которые я отправил, весят больше: адресат выбран мной.
const SENT_WEIGHT: u32 = 3;

impl AddressBook {
    /// Собрать из хранилища: отправители всех писем и получатели писем из «Отправленных».
    pub fn from_cache(cache: &Cache, me: &str) -> Self {
        let mut book = Self::default();
        let mut offset = 0;
        while let Ok(rows) = cache.all_rows(offset, 5000) {
            if rows.is_empty() {
                break;
            }
            offset += rows.len();
            for r in &rows {
                book.touch(&r.from_name, &r.from_addr, Source::Recent, 1, r.received_at);
            }
        }
        let sent = cache.mailboxes().unwrap_or_default().into_iter().find(|m| m.role.as_deref() == Some("sent"));
        if let Some(sent) = sent {
            let mut offset = 0;
            while let Ok(rows) = cache.page(&sent.id, offset, 2000) {
                if rows.is_empty() {
                    break;
                }
                offset += rows.len();
                for r in &rows {
                    let Ok(Some(body)) = cache.body(&r.id) else { continue };
                    for a in body.to.iter().chain(&body.cc) {
                        book.touch(&a.name, &a.email, Source::Recent, SENT_WEIGHT, r.received_at);
                    }
                }
            }
        }
        book.entries.remove(&me.to_lowercase());
        book
    }

    /// Добавить или усилить запись; более «официальный» источник и непустое имя побеждают.
    pub fn touch(&mut self, name: &str, email: &str, source: Source, weight: u32, at: i64) {
        let email = email.trim();
        if email.is_empty() || !email.contains('@') {
            return;
        }
        let key = email.to_lowercase();
        let e = self.entries.entry(key).or_insert_with(|| Entry {
            name: String::new(),
            email: email.to_string(),
            source,
            count: 0,
            last: 0,
            keys: Vec::new(),
        });
        e.count = e.count.saturating_add(weight);
        e.last = e.last.max(at);
        let better_name = !name.trim().is_empty() && (e.name.is_empty() || source > e.source);
        if source > e.source {
            e.source = source;
        }
        if better_name {
            e.name = name.trim().to_string();
        }
        e.keys = keys(&e.name, &e.email);
    }

    pub fn add_contacts<'a>(&mut self, items: impl IntoIterator<Item = (&'a str, &'a str)>) {
        for (name, email) in items {
            self.touch(name, email, Source::Contact, 0, 0);
        }
    }

    pub fn add_directory<'a>(&mut self, items: impl IntoIterator<Item = (&'a str, &'a str)>) {
        for (name, email) in items {
            self.touch(name, email, Source::Directory, 0, 0);
        }
    }

    /// Страница справочника (`Event::DirectoryPage`, T1-25б): люди с адресом — к уже известным, без дублей по адресу
    /// (повтор страницы и человек на двух страницах — одна запись).
    pub fn add_directory_page(&mut self, people: &[crate::model::Principal]) {
        self.add_directory(people.iter().filter_map(|p| Some((p.display_name(), p.email.as_deref()?))));
    }

    /// Влить записи другой книги (например, пришедшие раньше контакты с сервера).
    pub fn absorb(&mut self, other: AddressBook) {
        for e in other.entries.into_values() {
            self.touch(&e.name, &e.email, e.source, e.count, e.last);
        }
    }

    /// Убрать адрес (например, свой — себе писать не подсказываем).
    pub fn remove(&mut self, email: &str) {
        self.entries.remove(&email.to_lowercase());
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Запись адреса (регистр не важен).
    pub fn get(&self, email: &str) -> Option<&Entry> {
        self.entries.get(&email.trim().to_lowercase())
    }

    /// Все записи — для проверок перед отправкой (`crate::checks`).
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Подсказки для набранного; `now` — для учёта свежести.
    pub fn suggest(&self, query: &str, limit: usize, now: i64) -> Vec<&Entry> {
        let words: Vec<String> = normalize(query).split_whitespace().map(str::to_string).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let variants: Vec<Vec<String>> = words.iter().map(|w| variants(w)).collect();
        let mut hits: Vec<(f32, &Entry)> = self
            .entries
            .values()
            .filter_map(|e| {
                let mut quality = 0.0;
                for vs in &variants {
                    let best = vs
                        .iter()
                        .filter_map(|v| e.keys.iter().position(|k| k.starts_with(v.as_str())).map(|pos| if pos == 0 { 1.0 } else { 0.8 }))
                        .fold(0.0f32, f32::max);
                    if best == 0.0 {
                        return None;
                    }
                    quality += best;
                }
                let days = ((now - e.last).max(0) as f32) / 86_400.0;
                let fresh = if e.last > 0 { 1.0 + (-days / 90.0).exp() } else { 1.0 };
                let freq = 1.0 + (1.0 + e.count as f32).ln();
                let source = match e.source {
                    Source::Recent => 1.0,
                    Source::Contact => 1.3,
                    Source::Directory => 1.2,
                };
                Some((quality * freq * fresh * source, e))
            })
            .collect();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.email.cmp(&b.1.email)));
        hits.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

/// Окно «Часто пишу» (S5-25, макет «Контакты»): переписка за последние 30 дней.
pub const FREQUENT_DAYS: i64 = 30;

impl AddressBook {
    /// «Часто пишу» (S5-25): с кем была переписка за `days` дней — по частоте (отправленные весят больше, см.
    /// [`AddressBook::from_cache`]) и давности; один адрес — одна строка (справочник и личные контакты — та же запись
    /// по адресу). Не больше `limit`.
    pub fn frequent(&self, now: i64, days: i64, limit: usize) -> Vec<&Entry> {
        let since = now - days * 86_400;
        let mut hits: Vec<(f32, &Entry)> = self
            .entries
            .values()
            .filter(|e| e.count > 0 && e.last >= since)
            .map(|e| {
                let age = ((now - e.last).max(0) as f32) / 86_400.0;
                let score = (1.0 + (1.0 + e.count as f32).ln()) * (1.0 + (-age / days.max(1) as f32).exp());
                (score, e)
            })
            .collect();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.last.cmp(&a.1.last)).then_with(|| a.1.email.cmp(&b.1.email)));
        hits.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

/// Запрос подходит к имени и адресу так же, как в подсказках адреса: каждое слово — начало слова имени или адреса, в
/// любой раскладке и транслитом (поиск в «Контактах», S5-25).
pub fn matches(name: &str, email: &str, query: &str) -> bool {
    let keys = keys(name, email);
    let words: Vec<String> = normalize(query).split_whitespace().map(str::to_string).collect();
    !words.is_empty() && words.iter().all(|w| variants(w).iter().any(|v| keys.iter().any(|k| k.starts_with(v.as_str()))))
}

impl Entry {
    /// `Имя <адрес>` для вставки в поле.
    pub fn display(&self) -> String {
        if self.name.is_empty() { self.email.clone() } else { format!("{} <{}>", self.name, self.email) }
    }
}

fn normalize(s: &str) -> String {
    s.to_lowercase().replace('ё', "е")
}

/// Слова для поиска: части имени, части адреса (до @ по точкам/дефисам, домен), транслит имени.
fn keys(name: &str, email: &str) -> Vec<String> {
    let mut out = Vec::new();
    let name = normalize(name);
    for w in name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        out.push(w.to_string());
        let t = translit(w);
        if t != w {
            out.push(t);
        }
    }
    let email = normalize(email);
    out.push(email.clone());
    if let Some((local, domain)) = email.split_once('@') {
        out.extend(local.split(['.', '-', '_']).filter(|p| !p.is_empty()).map(str::to_string));
        out.push(domain.to_string());
    }
    out
}

/// Варианты набранного слова: как есть и в другой раскладке.
fn variants(word: &str) -> Vec<String> {
    let mut v = vec![word.to_string()];
    if let Some(sw) = switch_layout(word) {
        v.push(sw);
    }
    v
}

const QWERTY: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
const JCUKEN: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";

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

/// Транслит кириллицы латиницей (как пишут в адресах: Иванова → ivanova, Щукин → shchukin).
fn translit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let t = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'ё' | 'э' => "e",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "kh",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' | 'ь' => "",
            'ы' => "y",
            'ю' => "yu",
            'я' => "ya",
            c => {
                out.push(c);
                continue;
            }
        };
        out.push_str(t);
    }
    out
}

/// Справочник для людей: сотрудники, переговорные и ресурсы с адресом (без групп), по имени для людей
/// (`description`, а без него — `name`: у Stalwart `name` — логин).
pub fn directory(mut people: Vec<crate::model::Principal>) -> Vec<crate::model::Principal> {
    people.retain(|p| p.kind != "group" && p.email.as_deref().is_some_and(|e| e.contains('@')));
    people.sort_by_cached_key(|p| (p.display_name().to_lowercase(), p.email.clone()));
    people
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn book() -> AddressBook {
        let mut b = AddressBook::default();
        b.touch("Иванова Мария", "m.ivanova@corp.example", Source::Recent, 10, NOW - 86_400);
        b.touch("Иванов Пётр", "p.ivanov@corp.example", Source::Recent, 1, NOW - 400 * 86_400);
        b.touch("", "noreply@gitlab.corp.example", Source::Recent, 50, NOW);
        b.add_directory([("Щукин Алексей", "a.shchukin@corp.example")]);
        b
    }

    fn emails(b: &AddressBook, q: &str) -> Vec<String> {
        b.suggest(q, 5, NOW).into_iter().map(|e| e.email.clone()).collect()
    }

    #[test]
    fn prefix_frequency_and_recency() {
        let b = book();
        // Частая и недавняя Мария выше редкого и давнего Петра.
        assert_eq!(emails(&b, "иван"), ["m.ivanova@corp.example", "p.ivanov@corp.example"]);
        assert_eq!(emails(&b, "иван пет"), ["p.ivanov@corp.example"]);
        assert_eq!(emails(&b, "петр"), ["p.ivanov@corp.example"], "ё = е");
        assert_eq!(emails(&b, "gitlab"), ["noreply@gitlab.corp.example"]);
    }

    /// «Часто пишу»: частые и свежие выше; старше 30 дней и без переписки (только справочник) — нет; адрес — один раз.
    #[test]
    fn frequent_by_count_and_recency() {
        let mut b = AddressBook::default();
        let day = 86_400;
        b.touch("Мария", "m@x.ru", Source::Recent, 9, NOW - day);
        b.touch("Пётр", "p@x.ru", Source::Recent, 9, NOW - 20 * day);
        b.touch("Анна", "a@x.ru", Source::Recent, 1, NOW - day);
        b.touch("Старый", "old@x.ru", Source::Recent, 50, NOW - 40 * day);
        b.add_directory([("Мария Иванова", "M@X.RU"), ("Сотрудник", "dir@x.ru")]);
        b.add_contacts([("Анна Смирнова", "a@x.ru")]);
        let f: Vec<&str> = b.frequent(NOW, FREQUENT_DAYS, 10).into_iter().map(|e| e.email.as_str()).collect();
        assert_eq!(f, ["m@x.ru", "p@x.ru", "a@x.ru"]);
        assert_eq!(b.frequent(NOW, FREQUENT_DAYS, 10)[0].name, "Мария Иванова", "имя — из справочника");
        assert_eq!(b.frequent(NOW, FREQUENT_DAYS, 2).len(), 2);
        // Равные счёт и срок — по адресу.
        let mut b = AddressBook::default();
        b.touch("", "b@x.ru", Source::Recent, 2, NOW);
        b.touch("", "a@x.ru", Source::Recent, 2, NOW);
        let f: Vec<&str> = b.frequent(NOW, FREQUENT_DAYS, 10).into_iter().map(|e| e.email.as_str()).collect();
        assert_eq!(f, ["a@x.ru", "b@x.ru"]);
    }

    #[test]
    fn query_matches_names_layout_and_translit() {
        assert!(matches("Мария Иванова", "m.ivanova@corp.example", "иван"));
        assert!(matches("Мария Иванова", "m.ivanova@corp.example", "bdfy"), "английская раскладка");
        assert!(matches("Алексей Щукин", "a@x.ru", "shchuk"), "транслит");
        assert!(matches("Мария Иванова", "m.ivanova@corp.example", "мар ива"));
        assert!(!matches("Мария Иванова", "m.ivanova@corp.example", "петр"));
        assert!(!matches("Мария", "m@x.ru", "  "));
    }

    #[test]
    fn translit_and_layout() {
        let b = book();
        assert_eq!(emails(&b, "shchuk"), ["a.shchukin@corp.example"]);
        assert_eq!(emails(&b, "ivanova"), ["m.ivanova@corp.example"]);
        // «bdfyjdf» — «иванова», набранное в английской раскладке.
        assert_eq!(emails(&b, "bdfyjdf"), ["m.ivanova@corp.example"]);
        assert!(emails(&b, "сидоров").is_empty());
    }

    #[test]
    fn better_source_wins_name() {
        let mut b = AddressBook::default();
        b.touch("Mary", "m@x.ru", Source::Recent, 1, NOW);
        b.add_contacts([("Мария Иванова", "M@X.RU")]);
        assert_eq!(b.len(), 1);
        let e = &b.suggest("мария", 1, NOW)[0];
        assert_eq!(e.display(), "Мария Иванова <m@x.ru>");
        assert_eq!(e.source, Source::Contact);
    }

    /// Справочник по страницам (T1-25б): две страницы с общим человеком (адрес в другом регистре) — без дублей, их
    /// люди — в подсказках.
    #[test]
    fn directory_pages_are_merged_without_duplicates() {
        let p = |name: &str, email: Option<&str>| crate::model::Principal {
            id: name.into(),
            name: name.into(),
            email: email.map(str::to_string),
            kind: "individual".into(),
            ..Default::default()
        };
        let mut b = AddressBook::default();
        b.add_directory_page(&[p("Анна Смирнова", Some("anna@corp.example")), p("Без адреса", None), p("Борис", Some("boris@corp.example"))]);
        b.add_directory_page(&[p("Борис Петров", Some("Boris@corp.example")), p("Вера", Some("vera@corp.example"))]);
        assert_eq!(b.len(), 3);
        assert_eq!(emails(&b, "бор"), ["boris@corp.example"]);
        assert_eq!(b.get("boris@corp.example").map(|e| e.source), Some(Source::Directory));
        assert_eq!(emails(&b, "вера"), ["vera@corp.example"]);
        assert!(emails(&b, "без").is_empty());
    }

    #[test]
    fn directory_has_people_with_addresses() {
        let p = |name: &str, email: Option<&str>, kind: &str| crate::model::Principal {
            id: name.into(),
            name: name.into(),
            description: None,
            email: email.map(str::to_string),
            kind: kind.into(),
            ..Default::default()
        };
        // Как у Stalwart: `name` — логин, ФИО — в `description`; порядок — по ФИО.
        let login = crate::model::Principal {
            id: "z".into(),
            name: "z.boris@corp.example".into(),
            description: Some("Борисов Иван".into()),
            email: Some("z.boris@corp.example".into()),
            kind: "individual".into(),
            ..Default::default()
        };
        let people = directory(vec![
            p("Щукин Алексей", Some("a.shchukin@corp.example"), "individual"),
            p("Все сотрудники", Some("all@corp.example"), "group"),
            p("admin", None, "individual"),
            p("Переговорная 3", Some("room3@corp.example"), "location"),
            p("Анна", Some("anna@corp.example"), "individual"),
            login,
        ]);
        let names: Vec<&str> = people.iter().map(|p| p.display_name()).collect();
        assert_eq!(names, ["Анна", "Борисов Иван", "Переговорная 3", "Щукин Алексей"]);
    }
}
