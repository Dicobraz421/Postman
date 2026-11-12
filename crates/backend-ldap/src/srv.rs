//! Контроллер домена по DNS (macOS, MAC-05; на Windows — `DsGetDcNameW`): домен — область (realm) билета Kerberos
//! пользователя, контроллеры — записи SRV `_ldap._tcp.dc._msdcs.<домен>` (RFC 2782, как ищет их Windows). Здесь —
//! разбор ответа и выбор; запрос DNS и билет — в `sys`. Ответ DNS — чужие данные: разбор без паник.

/// Запись SRV (RFC 2782).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Srv {
    pub(crate) priority: u16,
    pub(crate) weight: u16,
    pub(crate) port: u16,
    /// Имя узла без точки на конце.
    pub(crate) target: String,
}

/// Порт LDAP по умолчанию: его в адрес не пишем.
const LDAP_PORT: u16 = 389;

/// Имя записей SRV контроллеров домена `domain`.
pub(crate) fn query_name(domain: &str) -> String {
    format!("_ldap._tcp.dc._msdcs.{domain}.")
}

/// Данные записи SRV (RDATA): приоритет, вес, порт, имя узла меткам (без сжатия — RFC 2782). Имя «.» — службы нет.
pub(crate) fn parse(rdata: &[u8]) -> Option<Srv> {
    let (head, mut name) = rdata.split_first_chunk::<6>()?;
    let [p0, p1, w0, w1, o0, o1] = *head;
    let mut labels = Vec::new();
    loop {
        let (&len, rest) = name.split_first()?;
        if len == 0 {
            break;
        }
        // Старшие биты — сжатие или расширенные метки: в SRV их быть не должно.
        if len & 0xC0 != 0 {
            return None;
        }
        let (label, rest) = rest.split_at_checked(usize::from(len))?;
        if !label.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_') {
            return None;
        }
        labels.push(std::str::from_utf8(label).ok()?);
        name = rest;
    }
    let target = labels.join(".");
    if target.is_empty() || target.len() > 253 {
        return None;
    }
    Some(Srv { priority: u16::from_be_bytes([p0, p1]), weight: u16::from_be_bytes([w0, w1]), port: u16::from_be_bytes([o0, o1]), target })
}

/// Сервер из записей: наименьший приоритет, при равном — наибольший вес (выбор постоянный: одному клиенту случайный
/// по RFC 2782 ни к чему). Порт — если не 389.
pub(crate) fn choose(records: impl IntoIterator<Item = Srv>) -> Option<String> {
    let best = records.into_iter().filter(|s| s.port != 0).min_by_key(|s| (s.priority, std::cmp::Reverse(s.weight)))?;
    Some(if best.port == LDAP_PORT { best.target } else { format!("{}:{}", best.target, best.port) })
}

/// Домен DNS из имени Kerberos `user@CORP.EXAMPLE`: область в нижнем регистре (у AD она и есть домен DNS).
pub(crate) fn realm_domain(principal: &str) -> Option<String> {
    let realm = principal.rsplit_once('@')?.1.trim().trim_end_matches('.');
    let valid = !realm.is_empty()
        && realm.len() <= 253
        && realm.split('.').all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    valid.then(|| realm.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn rdata(priority: u16, weight: u16, port: u16, name: &str) -> Vec<u8> {
        let mut out = [priority.to_be_bytes(), weight.to_be_bytes(), port.to_be_bytes()].concat();
        for l in name.split('.').filter(|l| !l.is_empty()) {
            out.push(l.len() as u8);
            out.extend_from_slice(l.as_bytes());
        }
        out.push(0);
        out
    }

    #[test]
    fn parses_srv_record() {
        let srv = parse(&rdata(0, 100, 389, "dc1.corp.example.")).unwrap();
        assert_eq!(srv, Srv { priority: 0, weight: 100, port: 389, target: "dc1.corp.example".into() });
    }

    #[test]
    fn rejects_bad_srv_records() {
        // Короче заголовка, без конца имени, метка за пределами, сжатие, «.», чужие символы.
        assert_eq!(parse(&[0, 0, 0, 1]), None);
        let whole = rdata(0, 0, 389, "dc1.corp.example");
        assert_eq!(parse(&whole[..whole.len() - 1]), None);
        assert_eq!(parse(&[0, 0, 0, 0, 1, 133, 9, b'd', b'c']), None);
        assert_eq!(parse(&[0, 0, 0, 0, 1, 133, 0xC0, 12]), None);
        assert_eq!(parse(&rdata(0, 0, 389, ".")), None);
        assert_eq!(parse(&rdata(0, 0, 389, "dc 1.corp.example")), None);
    }

    #[test]
    fn chooses_lowest_priority_then_highest_weight() {
        let records = [
            rdata(10, 100, 389, "far.corp.example"),
            rdata(0, 10, 389, "light.corp.example"),
            rdata(0, 50, 389, "dc2.corp.example"),
            rdata(0, 90, 0, "noport.corp.example"),
        ];
        assert_eq!(choose(records.iter().filter_map(|r| parse(r))), Some("dc2.corp.example".into()));
        assert_eq!(choose(parse(&rdata(0, 0, 3268, "gc.corp.example"))), Some("gc.corp.example:3268".into()));
        assert_eq!(choose(Vec::new()), None);
    }

    #[test]
    fn domain_from_kerberos_principal() {
        assert_eq!(realm_domain("ivanov@CORP.EXAMPLE"), Some("corp.example".into()));
        assert_eq!(realm_domain("host/mac@CORP.EXAMPLE."), Some("corp.example".into()));
        assert_eq!(realm_domain("ivanov"), None);
        assert_eq!(realm_domain("ivanov@"), None);
        assert_eq!(realm_domain("ivanov@CORP..EXAMPLE"), None);
        assert_eq!(realm_domain("ivanov@CORP EXAMPLE"), None);
        assert_eq!(query_name("corp.example"), "_ldap._tcp.dc._msdcs.corp.example.");
    }
}
