//! «Конфиденциально» = шифрование S/MIME (S5-12 Ф4, `docs/SMIME.md` §6.4, §7): сертификаты адресатов и свои —
//! кандидаты из кэша подписанных писем, «Других пользователей» и `My` (ядро), проверка и разбор — воркер
//! (`SmimePrepare`); здесь — книга найденного на сеанс, замки у адресатов, получатели для отправки.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use gpui_kit::*;
use postman_compose::Address;
use postman_core::checks::Family;
use postman_smime::prepare::{self, Prepared};
use postman_smime::seal::To;

use crate::mailer::Mailer;

/// Найденные сертификаты шифрования по адресам (нижний регистр).
#[derive(Default)]
pub(crate) struct CertBook {
    prepared: HashMap<String, Vec<Prepared>>,
    /// Адреса, которые искали (нет в `prepared` — сертификата нет).
    resolved: HashSet<String>,
    pending: HashSet<String>,
    /// Объявили AES-GCM.
    gcm: HashSet<String>,
    /// Растёт при каждом итоге поиска — окна писем пересчитывают замечания.
    pub(crate) version: u64,
}

pub(crate) type Certs = Arc<Mutex<CertBook>>;

pub(crate) fn lock(c: &Certs) -> std::sync::MutexGuard<'_, CertBook> {
    c.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CertBook {
    /// Семейства найденных сертификатов адреса; `None` — ещё не искали.
    pub(crate) fn families(&self, addr: &str) -> Option<Vec<Family>> {
        let addr = addr.trim().to_lowercase();
        let list = self.prepared.get(&addr);
        if list.is_none() && !self.resolved.contains(&addr) {
            return None;
        }
        let mut out = Vec::new();
        for p in list.into_iter().flatten() {
            let f = if p.key.is_gost() { Family::Gost } else { Family::Rsa };
            if !out.contains(&f) {
                out.push(f);
            }
        }
        Some(out)
    }

    /// Адреса, которых ещё не искали.
    pub(crate) fn unknown<'a>(&self, addrs: impl Iterator<Item = &'a str>) -> Vec<String> {
        addrs
            .map(|a| a.trim().to_lowercase())
            .filter(|a| !a.is_empty() && !self.resolved.contains(a) && !self.pending.contains(a))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn best(&self, addr: &str, gost: bool) -> Option<&Prepared> {
        let list: Vec<Prepared> = self.prepared.get(&addr.trim().to_lowercase())?.iter().filter(|p| p.key.is_gost() == gost).cloned().collect();
        let chosen = prepare::choose(&list, None)?.fingerprint.clone();
        self.prepared.get(&addr.trim().to_lowercase())?.iter().find(|p| p.fingerprint == chosen)
    }

    /// Получатели для отправки: у каждого адресата и у себя — лучший сертификат каждого семейства (делит письмо и
    /// выбирает ядро, `postman_core::smime::split_for_encryption`).
    pub(crate) fn encrypt_to(&self, recipients: &[Address], from: &str) -> Vec<To> {
        let mut out: Vec<To> = Vec::new();
        for a in recipients.iter().map(|a| a.email.as_str()).chain(std::iter::once(from)) {
            let gcm = self.gcm.contains(&a.trim().to_lowercase());
            for gost in [false, true] {
                if let Some(p) = self.best(a, gost)
                    && !out.iter().any(|t| t.prepared.fingerprint == p.fingerprint)
                {
                    out.push(To { prepared: p.clone(), gcm });
                }
            }
        }
        out
    }
}

impl Mailer {
    /// Найти сертификаты адресов `addrs` (и свои — `from`) в фоне: кандидаты — ядро, проверка — воркер.
    pub(crate) fn resolve_certs(&mut self, addrs: Vec<String>, from: String, cx: &mut Context<Self>) {
        let certs = self.compose.certs.clone();
        {
            let mut b = lock(&certs);
            for a in &addrs {
                b.pending.insert(a.clone());
            }
        }
        let live = self.backend.is_live();
        let (cache, worker) = (self.cache.clone(), self.reading.read(cx).worker.clone());
        let env = if live { postman_core::smime::Context::system() } else { crate::smime::demo_context() };
        let book = certs.clone();
        let task = cx.background_spawn(async move {
            let now = jiff::Timestamp::now().as_second();
            let mut candidates = postman_core::smime::recipient_candidates(&cache, &addrs, &from);
            // Демо: свой сертификат владельца демо-ящика (ключ — тестовый, `postman_demo::smime::my_key`).
            if !live {
                candidates.push(postman_demo::smime::my_key().0.to_vec());
            }
            let trust = env.anchors.trust();
            let prepared = if candidates.is_empty() {
                Ok(Vec::new())
            } else {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.smime_prepare(&candidates, &trust, u64::try_from(now).unwrap_or(0))
            };
            let gcm: Vec<String> = addrs.iter().filter(|a| postman_core::smime::declares_gcm(&cache, a)).cloned().collect();
            let mut b = lock(&book);
            match prepared {
                Ok(list) => {
                    for p in list.into_iter().flatten() {
                        for e in p.emails.clone() {
                            let slot = b.prepared.entry(e).or_default();
                            if !slot.iter().any(|x| x.fingerprint == p.fingerprint) {
                                slot.push(p.clone());
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("S/MIME recipients are not prepared: {e:?}"),
            }
            for a in addrs {
                b.pending.remove(&a);
                b.resolved.insert(a);
            }
            b.gcm.extend(gcm);
            b.version += 1;
        });
        cx.spawn(async move |this, cx| {
            task.await;
            this.update(cx, |m, cx| {
                for o in &m.compose.open {
                    o.view.update(cx, |_, cx| cx.notify());
                }
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: из `gpui_kit::*` пришёл бы атрибут `test` gpui вместо встроенного.
    use super::{CertBook, Family};
    use postman_compose::Address;
    use postman_smime::prepare::Prepared;

    fn prepared(email: &str, gost: bool, fp: &str, not_after: i64) -> Prepared {
        use postman_smime::envelope::GostKey;
        use postman_smime::prepare::RecipientKey;
        Prepared {
            issuer: vec![],
            serial: vec![1],
            ski: None,
            key: if gost {
                RecipientKey::Gost {
                    key: GostKey::G2012_256,
                    point: vec![0; 64],
                    public_param_set: "1.2.643.7.1.2.1.1.1".into(),
                    digest_param_set: None,
                }
            } else {
                RecipientKey::Rsa { n: vec![1], e: vec![1] }
            },
            emails: vec![email.into()],
            not_after,
            fingerprint: fp.into(),
        }
    }

    /// Семейства, «ещё не искали», лучший по сроку; себе — тоже.
    #[test]
    fn book() {
        let mut b = CertBook::default();
        b.prepared.insert("p@x".into(), vec![prepared("p@x", false, "A", 1), prepared("p@x", false, "B", 5), prepared("p@x", true, "G", 5)]);
        b.prepared.insert("me@x".into(), vec![prepared("me@x", false, "M", 5)]);
        b.resolved.extend(["p@x".to_string(), "none@x".to_string(), "me@x".to_string()]);
        assert_eq!(b.families("P@x"), Some(vec![Family::Rsa, Family::Gost]));
        assert_eq!(b.families("none@x"), Some(vec![]));
        assert_eq!(b.families("new@x"), None);
        assert_eq!(b.unknown(["new@x", "p@x", ""].into_iter()), vec!["new@x".to_string()]);
        let to = b.encrypt_to(&[Address::new("", "p@x")], "me@x");
        let fps: Vec<&str> = to.iter().map(|t| t.prepared.fingerprint.as_str()).collect();
        assert_eq!(fps, ["B", "G", "M"]);
    }
}
