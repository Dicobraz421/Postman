//! Типы `postman_jmap` ↔ нейтральная модель (`postman_core::model`), один к одному. Оба типа чужие для плагина,
//! поэтому перевод — своими трейтами ([`ToJmap`], [`FromJmap`]), а не `From`.

use postman_core::model::{Auth, ContactCard, Feature, Features, Filter, Principal, Release, VacationResponse};

/// Модель → тип JMAP.
pub trait ToJmap {
    type Jmap;
    fn to_jmap(&self) -> Self::Jmap;
}

/// Тип JMAP → модель.
pub trait FromJmap<T> {
    fn from_jmap(t: T) -> Self;
}

/// Что умеет сервер JMAP — по capabilities сессии. Задачи живут в календаре (S6-01), поэтому идут за ним.
pub fn features(client: &postman_jmap::Client) -> Features {
    features_of(&|c| client.has(c))
}

fn features_of(has: &dyn Fn(&str) -> bool) -> Features {
    use postman_jmap::{CALENDARS, CONTACTS, PRINCIPALS, PRINCIPALS_AVAILABILITY, QUOTA, SIEVE, VACATION_RESPONSE};
    [
        (Feature::Calendar, CALENDARS),
        (Feature::Tasks, CALENDARS),
        (Feature::Contacts, CONTACTS),
        (Feature::Directory, PRINCIPALS),
        (Feature::FreeBusy, PRINCIPALS_AVAILABILITY),
        (Feature::Rules, SIEVE),
        (Feature::Autoreply, VACATION_RESPONSE),
        (Feature::Quota, QUOTA),
    ]
    .into_iter()
    .fold(Features::NONE, |f, (feature, capability)| f.with(feature, has(capability)))
}

impl ToJmap for Auth {
    type Jmap = postman_jmap::Auth;
    fn to_jmap(&self) -> postman_jmap::Auth {
        match self {
            Auth::Basic { user, password } => postman_jmap::Auth::Basic { user: user.clone(), password: password.clone() },
            Auth::Bearer(t) => postman_jmap::Auth::Bearer(t.clone()),
        }
    }
}

impl FromJmap<postman_jmap::Auth> for Auth {
    fn from_jmap(a: postman_jmap::Auth) -> Self {
        match a {
            postman_jmap::Auth::Basic { user, password } => Auth::Basic { user, password },
            postman_jmap::Auth::Bearer(t) => Auth::Bearer(t),
        }
    }
}

impl ToJmap for Filter {
    type Jmap = postman_jmap::Filter;
    fn to_jmap(&self) -> postman_jmap::Filter {
        use postman_jmap::Filter as J;
        let all = |v: &[Filter]| v.iter().map(Filter::to_jmap).collect();
        match self {
            Filter::InMailbox(s) => J::InMailbox(s.clone()),
            Filter::HasKeyword(s) => J::HasKeyword(s.clone()),
            Filter::NotKeyword(s) => J::NotKeyword(s.clone()),
            Filter::Text(s) => J::Text(s.clone()),
            Filter::From(s) => J::From(s.clone()),
            Filter::To(s) => J::To(s.clone()),
            Filter::Body(s) => J::Body(s.clone()),
            Filter::Subject(s) => J::Subject(s.clone()),
            Filter::HasAttachment(b) => J::HasAttachment(*b),
            Filter::And(v) => J::And(all(v)),
            Filter::Or(v) => J::Or(all(v)),
            Filter::Not(v) => J::Not(all(v)),
            Filter::Before(s) => J::Before(s.clone()),
            Filter::After(s) => J::After(s.clone()),
            Filter::MinSize(n) => J::MinSize(*n),
        }
    }
}

impl ToJmap for Release {
    type Jmap = postman_jmap::Release;
    fn to_jmap(&self) -> postman_jmap::Release {
        match *self {
            Release::Now => postman_jmap::Release::Now,
            Release::After(s) => postman_jmap::Release::After(s),
            Release::At(t) => postman_jmap::Release::At(t),
        }
    }
}

impl FromJmap<postman_jmap::Release> for Release {
    fn from_jmap(r: postman_jmap::Release) -> Self {
        match r {
            postman_jmap::Release::Now => Release::Now,
            postman_jmap::Release::After(s) => Release::After(s),
            postman_jmap::Release::At(t) => Release::At(t),
        }
    }
}

impl FromJmap<postman_jmap::ContactCard> for ContactCard {
    fn from_jmap(c: postman_jmap::ContactCard) -> Self {
        let postman_jmap::ContactCard { id, name, emails, phones, organization, title } = c;
        ContactCard { id, name, emails, phones, organization, title }
    }
}

impl ToJmap for ContactCard {
    type Jmap = postman_jmap::ContactCard;
    fn to_jmap(&self) -> postman_jmap::ContactCard {
        let ContactCard { id, name, emails, phones, organization, title } = self.clone();
        postman_jmap::ContactCard { id, name, emails, phones, organization, title }
    }
}

impl FromJmap<postman_jmap::Principal> for Principal {
    fn from_jmap(p: postman_jmap::Principal) -> Self {
        // Должности, отдела и телефона у JMAP Principal (RFC 9670) нет — пусто.
        let postman_jmap::Principal { id, name, description, email, kind } = p;
        Principal { id, name, description, email, kind, ..Principal::default() }
    }
}

impl FromJmap<postman_jmap::VacationResponse> for VacationResponse {
    fn from_jmap(v: postman_jmap::VacationResponse) -> Self {
        let postman_jmap::VacationResponse { is_enabled, from_date, to_date, subject, text_body, html_body } = v;
        VacationResponse { is_enabled, from_date, to_date, subject, text_body, html_body }
    }
}

impl ToJmap for VacationResponse {
    type Jmap = postman_jmap::VacationResponse;
    fn to_jmap(&self) -> postman_jmap::VacationResponse {
        let VacationResponse { is_enabled, from_date, to_date, subject, text_body, html_body } = self.clone();
        postman_jmap::VacationResponse { is_enabled, from_date, to_date, subject, text_body, html_body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_maps_to_the_same_jmap_query() {
        let f = Filter::And(vec![
            Filter::InMailbox("m1".into()),
            Filter::Or(vec![Filter::HasKeyword("pm.due".into()), Filter::NotKeyword("$seen".into())]),
            Filter::Not(vec![Filter::From("@example.test".into())]),
            Filter::Text("t".into()),
            Filter::To("a".into()),
            Filter::Body("b".into()),
            Filter::Subject("s".into()),
            Filter::HasAttachment(true),
            Filter::Before("2026-10-21T00:00:00Z".into()),
            Filter::After("2026-10-01T00:00:00Z".into()),
            Filter::MinSize(1024),
        ]);
        let j = postman_jmap::Filter::And(vec![
            postman_jmap::Filter::InMailbox("m1".into()),
            postman_jmap::Filter::Or(vec![postman_jmap::Filter::HasKeyword("pm.due".into()), postman_jmap::Filter::NotKeyword("$seen".into())]),
            postman_jmap::Filter::Not(vec![postman_jmap::Filter::From("@example.test".into())]),
            postman_jmap::Filter::Text("t".into()),
            postman_jmap::Filter::To("a".into()),
            postman_jmap::Filter::Body("b".into()),
            postman_jmap::Filter::Subject("s".into()),
            postman_jmap::Filter::HasAttachment(true),
            postman_jmap::Filter::Before("2026-10-21T00:00:00Z".into()),
            postman_jmap::Filter::After("2026-10-01T00:00:00Z".into()),
            postman_jmap::Filter::MinSize(1024),
        ]);
        assert_eq!(f.to_jmap(), j);
        assert_eq!(f.to_jmap().to_json(), j.to_json());
    }

    /// Условия строки поиска (`postman_core::query`, S5-29) — тот же `Email/query`, что был до плагинов.
    #[test]
    fn search_query_becomes_min_size() {
        let q = postman_core::query::Query::parse("has:attachment larger:5M");
        let env = q.env(&jiff::Zoned::now(), |_| None, &[]);
        let json = q.server_filter(&env).unwrap().to_jmap().to_json();
        assert!(json["conditions"].as_array().unwrap().iter().any(|c| c["minSize"] == 5 * 1024 * 1024), "{json}");
    }

    #[test]
    fn features_follow_capabilities() {
        assert_eq!(features_of(&|_| true), Features::ALL);
        assert_eq!(features_of(&|_| false), Features::NONE);
        let mail_only = features_of(&|c| c == postman_jmap::CORE || c == postman_jmap::MAIL);
        assert!(Feature::ALL.iter().all(|&f| !mail_only.has(f)));
        let no_sieve = features_of(&|c| c != postman_jmap::SIEVE);
        assert!(!no_sieve.has(Feature::Rules) && no_sieve.has(Feature::Autoreply) && no_sieve.has(Feature::Tasks));
    }

    #[test]
    fn release_and_cards_round_trip() {
        for r in [Release::Now, Release::After(13), Release::At(1_792_562_400)] {
            assert_eq!(Release::from_jmap(r.to_jmap()), r);
        }
        let card = ContactCard {
            id: "c1".into(),
            name: "Анна".into(),
            emails: vec!["a@example.test".into()],
            phones: vec!["+7 900".into()],
            organization: "ИТТ".into(),
            title: "инженер".into(),
        };
        assert_eq!(ContactCard::from_jmap(card.to_jmap()), card);
        // Патч карточки — по-прежнему только изменённые свойства (форма «Контакты», `app-gpui/src/contacts.rs`).
        let edited = ContactCard { phones: Vec::new(), title: "Директор".into(), ..card.clone() };
        let patch = edited.to_jmap().patch(&card.to_jmap());
        let mut keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["phones", "titles"]);
        assert!(patch["phones"].is_null(), "убранный телефон — null");
        assert_eq!(patch["titles"]["t1"]["name"], "Директор");
        let v = VacationResponse { is_enabled: true, subject: Some("Нет".into()), ..Default::default() };
        assert_eq!(VacationResponse::from_jmap(v.to_jmap()), v);
    }

    /// Разбор JSContact в модели (для CardDAV) — тот же, что у JMAP.
    #[test]
    fn jscontact_is_parsed_the_same_way() {
        let v = serde_json::json!({
            "id": "c1", "name": { "full": "Анна Иванова" },
            "emails": { "e2": { "address": "b@example.test" }, "e1": { "address": "a@example.test", "pref": 1 } },
            "organizations": { "o1": { "name": "ИТТ" } }, "titles": { "t1": { "kind": "role", "name": "x" } }
        });
        assert_eq!(ContactCard::from_jscontact(&v), ContactCard::from_jmap(postman_jmap::ContactCard::from_jscontact(&v)));
    }
}
