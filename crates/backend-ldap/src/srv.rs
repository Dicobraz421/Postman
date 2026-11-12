//! Контроллер домена по DNS (macOS, MAC-05; Linux, LIN-05; на Windows — `DsGetDcNameW`): домен — область (realm) билета Kerberos
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

/// Имя DNS в сообщении `msg` с позиции `at` — метками без сжатия (с нулём на конце); указатели сжатия (RFC 1035
/// §4.1.4) раскрываются, не больше 16 переходов. Второе — позиция после имени в исходном месте.
fn read_name(msg: &[u8], mut at: usize) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut end = None;
    for _ in 0..16 {
        loop {
            let len = *msg.get(at)?;
            match len & 0xC0 {
                0 if len == 0 => {
                    out.push(0);
                    return Some((out, end.unwrap_or(at + 1)));
                }
                0 => {
                    let label = msg.get(at..at + 1 + usize::from(len))?;
                    out.extend_from_slice(label);
                    if out.len() > 255 {
                        return None;
                    }
                    at += 1 + usize::from(len);
                }
                0xC0 => {
                    let low = *msg.get(at + 1)?;
                    end.get_or_insert(at + 2);
                    at = usize::from(len & 0x3F) << 8 | usize::from(low);
                    break;
                }
                _ => return None,
            }
        }
    }
    None
}

/// Данные записей типа `kind` из ответа DNS `msg` (RFC 1035 §4.1) — для SRV с именем узла без сжатия, как ждёт
/// [`parse`]. Повреждённый ответ — сколько успели разобрать.
pub(crate) fn answers(msg: &[u8], kind: u16) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(head) = msg.first_chunk::<12>() else { return out };
    let (questions, count) = (u16::from_be_bytes([head[4], head[5]]), u16::from_be_bytes([head[6], head[7]]));
    let mut at = 12;
    for _ in 0..questions {
        let Some((_, next)) = read_name(msg, at) else { return out };
        at = next + 4;
    }
    for _ in 0..count {
        let Some((_, next)) = read_name(msg, at) else { return out };
        let Some(fixed) = msg.get(next..next + 10) else { return out };
        let rtype = u16::from_be_bytes([fixed[0], fixed[1]]);
        let len = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        let start = next + 10;
        let Some(rdata) = msg.get(start..start + len) else { return out };
        if rtype == kind {
            if kind == 33 && rdata.len() > 6 {
                // Имя узла SRV — раскрыть возможное сжатие (некоторые серверы сжимают вопреки RFC 2782).
                if let Some((name, _)) = read_name(msg, start + 6) {
                    let mut r = rdata[..6].to_vec();
                    r.extend_from_slice(&name);
                    out.push(r);
                }
            } else {
                out.push(rdata.to_vec());
            }
        }
        at = start + len;
    }
    out
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

    /// Ответ DNS: вопрос, запись CNAME (пропускается), две SRV — одна со сжатым именем узла; обрезанный ответ — без
    /// паники.
    #[test]
    fn parses_dns_answer() {
        let mut msg = vec![0, 1, 0x81, 0x80, 0, 1, 0, 3, 0, 0, 0, 0];
        let qname = 12;
        msg.extend_from_slice(b"\x05_ldap\x04_tcp\x04corp\x07example\x00");
        msg.extend_from_slice(&[0, 33, 0, 1]);
        let ptr = |off: usize| [0xC0 | (off >> 8) as u8, off as u8];
        // CNAME
        msg.extend_from_slice(&ptr(qname));
        msg.extend_from_slice(&[0, 5, 0, 1, 0, 0, 0, 60, 0, 2]);
        msg.extend_from_slice(&ptr(qname));
        // SRV без сжатия
        msg.extend_from_slice(&ptr(qname));
        let plain = rdata(0, 100, 389, "dc1.corp.example");
        msg.extend_from_slice(&[0, 33, 0, 1, 0, 0, 0, 60, 0, plain.len() as u8]);
        msg.extend_from_slice(&plain);
        // SRV: «dc2» + указатель на «corp.example» в вопросе
        msg.extend_from_slice(&ptr(qname));
        let mut packed = vec![0, 0, 0, 200, 1, 0x85, 3, b'd', b'c', b'2'];
        packed.extend_from_slice(&ptr(qname + 11));
        msg.extend_from_slice(&[0, 33, 0, 1, 0, 0, 0, 60, 0, packed.len() as u8]);
        msg.extend_from_slice(&packed);
        let found: Vec<Srv> = answers(&msg, 33).iter().filter_map(|r| parse(r)).collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[1].target, "dc2.corp.example");
        assert_eq!(choose(found), Some("dc2.corp.example".into()));
        for cut in 0..msg.len() {
            let _ = answers(&msg[..cut], 33);
        }
        // Петля указателей — не зависает.
        let mut looped = vec![0, 1, 0x81, 0x80, 0, 0, 0, 1, 0, 0, 0, 0, 0xC0, 12];
        looped.extend_from_slice(&[0, 33, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert!(answers(&looped, 33).is_empty());
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
