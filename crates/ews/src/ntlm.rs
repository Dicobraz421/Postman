//! Вход с паролем — NTLMv2 на чистом Rust (MS-NLMP), одинаково на Windows и macOS: явный логин и пароль от SSPI
//! Windows и GSS.framework не зависят. Вход без пароля (учётная запись Windows, Kerberos) — `sys`.
//!
//! Логин — `DOMAIN\user` (домен NetBIOS) или UPN `user@domain`: UPN уходит целиком в имени пользователя с пустым
//! доменом, как у SSPI Windows. Сервер отклонил адрес почты (UPN в AD часто другой) — [`Ntlm::alternative`]:
//! `ДОМЕН\имя` с доменом NetBIOS из CHALLENGE сервера.
//!
//! В AUTHENTICATE: ответ NTLMv2, `MsvAvTargetName` (SPN `HTTP/<host>`), привязка к каналу TLS — MD5
//! `gss_channel_bindings` с данными `tls-server-end-point` (`MsvAvChannelBindings`, Extended Protection у Exchange),
//! MIC, если сервер прислал отметку времени. Без подписи и шифрования сеанса (HTTP они не нужны): ни
//! `NEGOTIATE_KEY_EXCH`, ни `SIGN`/`SEAL`.

use hmac::{Hmac, KeyInit as _, Mac as _};
use md4::Digest as _;
use zeroize::Zeroizing;

use crate::{Error, Result};

const UNICODE: u32 = 0x0000_0001;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NTLM: u32 = 0x0000_0200;
const ALWAYS_SIGN: u32 = 0x0000_8000;
const EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const TARGET_INFO: u32 = 0x0080_0000;
const VERSION: u32 = 0x0200_0000;
const KEY_128: u32 = 0x2000_0000;
const KEY_56: u32 = 0x8000_0000;
/// Флаги клиента; в AUTHENTICATE — их пересечение с флагами CHALLENGE.
const FLAGS: u32 = UNICODE | REQUEST_TARGET | NTLM | ALWAYS_SIGN | EXTENDED_SESSIONSECURITY | TARGET_INFO | VERSION | KEY_128 | KEY_56;
/// Версия клиента в сообщениях: Windows 10 (10.0.17763), `NTLMSSP_REVISION_W2K3`.
const OS_VERSION: [u8; 8] = [10, 0, 0x63, 0x45, 0, 0, 0, 15];

// AV-пары TargetInfo.
const AV_EOL: u16 = 0;
const AV_NB_DOMAIN: u16 = 2;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
const AV_SINGLE_HOST: u16 = 8;
const AV_TARGET_NAME: u16 = 9;
const AV_CHANNEL_BINDINGS: u16 = 10;
/// `MsvAvFlags`: в AUTHENTICATE есть MIC.
const AV_FLAG_MIC: u32 = 0x0000_0002;

/// Заголовок AUTHENTICATE: поля, флаги, версия, MIC.
const AUTH_HEADER: usize = 88;
const MIC_AT: usize = 72;

type HmacMd5 = Hmac<md5::Md5>;

fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    // HMAC принимает ключ любой длины.
    let Ok(mut mac) = HmacMd5::new_from_slice(key) else { return [0; 16] };
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn from_utf16(b: &[u8]) -> String {
    String::from_utf16_lossy(&b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>())
}

/// Логин → (пользователь, домен), как их делит SSPI Windows: `DOMAIN\user` → (`user`, `DOMAIN`); UPN и просто имя —
/// целиком, домен пустой. Пробелы по краям — прочь; пустой логин — ошибка.
pub(crate) fn split_login(login: &str) -> Result<(String, String)> {
    let login = login.trim();
    let (user, domain) = match login.split_once('\\') {
        Some((domain, user)) => (user.trim(), domain.trim()),
        None => (login, ""),
    };
    if user.is_empty() {
        return Err(Error::Auth("empty user name".into()));
    }
    Ok((user.to_owned(), domain.to_owned()))
}

/// Отметка времени Windows (FILETIME, сотни нс с 1601 года).
fn filetime_now() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (since.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(since.subsec_nanos() / 100)
}

/// Поле `(len, maxlen, offset)` сообщения NTLM по смещению `at`.
fn field(m: &[u8], at: usize) -> Option<&[u8]> {
    let h = m.get(at..at + 8)?;
    let len = u16::from_le_bytes([h[0], h[1]]) as usize;
    let off = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    m.get(off..off.checked_add(len)?)
}

/// AV-пары до `MsvAvEOL`.
fn av_pairs(mut info: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while let Some(h) = info.get(..4) {
        let id = u16::from_le_bytes([h[0], h[1]]);
        let len = u16::from_le_bytes([h[2], h[3]]) as usize;
        let Some(value) = info.get(4..4 + len) else { break };
        if id == AV_EOL {
            break;
        }
        out.push((id, value));
        info = &info[4 + len..];
    }
    out
}

fn push_av(out: &mut Vec<u8>, id: u16, value: &[u8]) {
    out.extend(id.to_le_bytes());
    out.extend((value.len() as u16).to_le_bytes());
    out.extend(value);
}

/// CHALLENGE сервера.
struct Challenge<'a> {
    flags: u32,
    server: [u8; 8],
    info: &'a [u8],
}

fn parse_challenge(m: &[u8]) -> Result<Challenge<'_>> {
    let bad = || Error::Auth("ntlm: malformed CHALLENGE from the server".into());
    if !m.starts_with(b"NTLMSSP\0\x02\0\0\0") {
        return Err(bad());
    }
    let flags = u32::from_le_bytes(m.get(20..24).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
    let server: [u8; 8] = m.get(24..32).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    let info = if flags & TARGET_INFO != 0 { field(m, 40).ok_or_else(bad)? } else { &[] };
    Ok(Challenge { flags, server, info })
}

/// MD5 `gss_channel_bindings` с пустыми адресами (тип 0) и данными приложения — как у SSPI Windows.
pub(crate) fn binding_hash(app: &[u8]) -> [u8; 16] {
    let mut flat = vec![0u8; 16];
    flat.extend((app.len() as u32).to_le_bytes());
    flat.extend(app);
    md5::Md5::digest(&flat).into()
}

/// Контекст NTLM одного рукопожатия.
pub(crate) struct Ntlm {
    user: String,
    domain: String,
    /// NT-хэш пароля (MD4 UTF-16LE).
    nt_hash: Zeroizing<[u8; 16]>,
    /// SPN `HTTP/<host>`.
    target: String,
    /// Данные приложения привязки к каналу (`tls-server-end-point:` + хэш).
    binding: Option<Vec<u8>>,
    /// Отправленное NEGOTIATE — для MIC.
    negotiate: Vec<u8>,
    /// Домен NetBIOS из CHALLENGE — для [`Ntlm::alternative`].
    nb_domain: Option<String>,
    pub(crate) done: bool,
}

impl Ntlm {
    /// Учётные данные и контекст для `target` (SPN `HTTP/<host>`); `binding` — данные приложения привязки к каналу.
    pub(crate) fn new(login: &str, password: &str, target: &str, binding: Option<&[u8]>) -> Result<Ntlm> {
        let (user, domain) = split_login(login)?;
        let pw = Zeroizing::new(utf16(password));
        let nt_hash = Zeroizing::new(md4::Md4::digest(&*pw).into());
        Ok(Ntlm {
            user,
            domain,
            nt_hash,
            target: target.to_owned(),
            binding: binding.map(<[u8]>::to_vec),
            negotiate: Vec::new(),
            nb_domain: None,
            done: false,
        })
    }

    /// Первый шаг (`None`) — NEGOTIATE; второй — CHALLENGE сервера → AUTHENTICATE.
    pub(crate) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>> {
        match (input, self.negotiate.is_empty(), self.done) {
            (None, true, false) => {
                let mut m = b"NTLMSSP\0".to_vec();
                m.extend(1u32.to_le_bytes());
                m.extend(FLAGS.to_le_bytes());
                // Домен и рабочая станция — пустые, данные — сразу за заголовком (40 байт).
                for _ in 0..2 {
                    m.extend([0u8, 0, 0, 0]);
                    m.extend(40u32.to_le_bytes());
                }
                m.extend(OS_VERSION);
                self.negotiate = m.clone();
                Ok(m)
            }
            (Some(challenge), false, false) => {
                let mut client = [0u8; 8];
                getrandom::fill(&mut client).map_err(|e| Error::Auth(format!("ntlm: random: {e}")))?;
                let m = self.authenticate(challenge, client, filetime_now())?;
                self.done = true;
                Ok(m)
            }
            _ => Err(Error::Auth("ntlm: unexpected handshake step".into())),
        }
    }

    /// AUTHENTICATE на `challenge` с вызовом клиента `client` и временем `now` (если у сервера нет своего).
    fn authenticate(&mut self, challenge: &[u8], client: [u8; 8], now: u64) -> Result<Vec<u8>> {
        let ch = parse_challenge(challenge)?;
        if ch.flags & UNICODE == 0 {
            return Err(Error::Auth("ntlm: server does not offer Unicode".into()));
        }
        let flags = ch.flags & FLAGS;
        let pairs = av_pairs(ch.info);
        self.nb_domain = pairs.iter().find(|(id, _)| *id == AV_NB_DOMAIN).map(|(_, v)| from_utf16(v)).filter(|d| !d.is_empty());
        let timestamp = pairs.iter().find(|(id, _)| *id == AV_TIMESTAMP).and_then(|(_, v)| <[u8; 8]>::try_from(*v).ok());
        let time = timestamp.map_or(now, u64::from_le_bytes);

        // AV-пары ответа: пары сервера, флаги (MIC), SPN, привязка к каналу.
        let mut info = Vec::new();
        let mut av_flags = 0u32;
        for (id, v) in &pairs {
            match *id {
                AV_FLAGS => av_flags = v.get(..4).and_then(|b| b.try_into().ok()).map_or(0, u32::from_le_bytes),
                AV_SINGLE_HOST | AV_TARGET_NAME | AV_CHANNEL_BINDINGS => {}
                _ => push_av(&mut info, *id, v),
            }
        }
        if timestamp.is_some() {
            av_flags |= AV_FLAG_MIC;
        }
        if av_flags != 0 {
            push_av(&mut info, AV_FLAGS, &av_flags.to_le_bytes());
        }
        push_av(&mut info, AV_TARGET_NAME, &utf16(&self.target));
        if let Some(app) = &self.binding {
            push_av(&mut info, AV_CHANNEL_BINDINGS, &binding_hash(app));
        }
        push_av(&mut info, AV_EOL, &[]);

        let mut temp = vec![1u8, 1, 0, 0, 0, 0, 0, 0];
        temp.extend(time.to_le_bytes());
        temp.extend(client);
        temp.extend([0u8; 4]);
        temp.extend(&info);
        temp.extend([0u8; 4]);

        let ntowf = Zeroizing::new(ntowf_v2(&self.nt_hash, &self.user, &self.domain));
        let proof = hmac_md5(&*ntowf, &[&ch.server, &temp]);
        let mut nt = proof.to_vec();
        nt.extend(&temp);
        // С отметкой времени сервера LMv2 не шлют (MS-NLMP 3.1.5.1.2) — нули.
        let lm = if timestamp.is_some() {
            vec![0u8; 24]
        } else {
            let mut lm = hmac_md5(&*ntowf, &[&ch.server, &client]).to_vec();
            lm.extend(client);
            lm
        };
        let session = Zeroizing::new(hmac_md5(&*ntowf, &[&proof]));

        let (domain, user) = (utf16(&self.domain), utf16(&self.user));
        let mut m = vec![0u8; AUTH_HEADER];
        m[..12].copy_from_slice(b"NTLMSSP\0\x03\0\0\0");
        let mut off = AUTH_HEADER;
        // Поля: LM 12, NT 20, домен 28, пользователь 36, рабочая станция 44, ключ сеанса 52; данные — в порядке
        // домен, пользователь, рабочая станция (пусто), LM, NT.
        let mut put = |m: &mut Vec<u8>, at: usize, data: &[u8]| {
            m[at..at + 2].copy_from_slice(&(data.len() as u16).to_le_bytes());
            m[at + 2..at + 4].copy_from_slice(&(data.len() as u16).to_le_bytes());
            m[at + 4..at + 8].copy_from_slice(&(off as u32).to_le_bytes());
            m.extend(data);
            off += data.len();
        };
        put(&mut m, 28, &domain);
        put(&mut m, 36, &user);
        put(&mut m, 44, &[]);
        put(&mut m, 12, &lm);
        put(&mut m, 20, &nt);
        put(&mut m, 52, &[]);
        m[60..64].copy_from_slice(&flags.to_le_bytes());
        m[64..72].copy_from_slice(&OS_VERSION);
        if timestamp.is_some() {
            let mic = hmac_md5(&*session, &[&self.negotiate, challenge, &m]);
            m[MIC_AT..MIC_AT + 16].copy_from_slice(&mic);
        }
        Ok(m)
    }

    /// Логин на вторую попытку, если сервер отклонил первую: вход был адресом почты (`имя@домен`) — `ДОМЕН\имя` с
    /// доменом NetBIOS из CHALLENGE. Иначе — нет.
    pub(crate) fn alternative(&self) -> Option<String> {
        let local = self.user.split_once('@').map(|(l, _)| l).filter(|l| !l.is_empty() && self.domain.is_empty())?;
        self.nb_domain.as_ref().map(|d| format!("{d}\\{local}"))
    }
}

/// `NTOWFv2` (MS-NLMP 3.3.2): HMAC-MD5 NT-хэша по `UPPER(user) + domain` в UTF-16LE.
fn ntowf_v2(nt_hash: &[u8; 16], user: &str, domain: &str) -> [u8; 16] {
    hmac_md5(nt_hash, &[&utf16(&user.to_uppercase()), &utf16(domain)])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    /// CHALLENGE условного Exchange (IIS): домен `EXAMPLE`, узел `mail.example.test`, отметка времени — для MIC.
    pub(crate) fn challenge() -> Vec<u8> {
        let target = utf16("EXAMPLE");
        let mut info = Vec::new();
        for (id, value) in [(2u16, utf16("EXAMPLE")), (1, utf16("MAIL")), (4, utf16("example.test")), (3, utf16("mail.example.test"))] {
            push_av(&mut info, id, &value);
        }
        push_av(&mut info, AV_TIMESTAMP, &133_000_000_000_000_000u64.to_le_bytes());
        push_av(&mut info, AV_EOL, &[]);
        let header = 56u32;
        let mut m = b"NTLMSSP\0".to_vec();
        m.extend(2u32.to_le_bytes());
        m.extend((target.len() as u16).to_le_bytes());
        m.extend((target.len() as u16).to_le_bytes());
        m.extend(header.to_le_bytes());
        m.extend(0xE289_8215u32.to_le_bytes()); // UNICODE, NTLM, ESS, TARGET_INFO, VERSION, 128, 56, KEY_EXCH…
        m.extend(SERVER_CHALLENGE);
        m.extend([0u8; 8]);
        m.extend((info.len() as u16).to_le_bytes());
        m.extend((info.len() as u16).to_le_bytes());
        m.extend((header + target.len() as u32).to_le_bytes());
        m.extend(OS_VERSION);
        m.extend(target);
        m.extend(info);
        m
    }

    /// AUTHENTICATE глазами контроллера домена: пользователь, домен, сходится ли NTProofStr с паролем, AV-пары
    /// ответа.
    pub(crate) fn verify(m: &[u8], password: &str) -> (String, String, bool, Vec<(u16, Vec<u8>)>) {
        assert!(m.starts_with(b"NTLMSSP\0\x03\0\0\0"), "AUTHENTICATE: {:?}", &m[..m.len().min(12)]);
        let nt = field(m, 20).unwrap();
        let (domain, user) = (from_utf16(field(m, 28).unwrap()), from_utf16(field(m, 36).unwrap()));
        let ntowf = ntowf_v2(&md4::Md4::digest(utf16(password)).into(), &user, &domain);
        let (proof, blob) = nt.split_at(16);
        let ok = hmac_md5(&ntowf, &[&SERVER_CHALLENGE, blob]) == proof;
        let pairs = av_pairs(&blob[28..]).into_iter().map(|(id, v)| (id, v.to_vec())).collect();
        (user, domain, ok, pairs)
    }

    #[test]
    fn logins() {
        assert_eq!(split_login(r"  CORP\ivanov ").unwrap(), ("ivanov".into(), "CORP".into()));
        assert_eq!(split_login("ivanov@corp.example").unwrap(), ("ivanov@corp.example".into(), String::new()), "UPN — целиком");
        assert_eq!(split_login("ivanov").unwrap(), ("ivanov".into(), String::new()));
        assert!(split_login("   ").is_err() && split_login(r"CORP\").is_err());
    }

    /// Пример NTLMv2 из MS-NLMP 4.2.4: User / Domain / Password, вызов клиента `aa…`, время 0.
    #[test]
    fn ms_nlmp_vectors() {
        let nt_hash: [u8; 16] = md4::Md4::digest(utf16("Password")).into();
        assert_eq!(nt_hash.to_vec(), hex("a4 f4 9c 40 65 10 bd ca b6 82 4e e7 c3 0f d8 52"));
        let ntowf = ntowf_v2(&nt_hash, "User", "Domain");
        assert_eq!(ntowf.to_vec(), hex("0c 86 8a 40 3b fd 7a 93 a3 00 1e f2 2e f0 2e 3f"));
        let client = [0xaa; 8];
        let lm = hmac_md5(&ntowf, &[&SERVER_CHALLENGE, &client]);
        assert_eq!(lm.to_vec(), hex("86 c3 50 97 ac 9c ec 10 25 54 76 4a 57 cc cc 19"));
        let mut info = Vec::new();
        push_av(&mut info, 2, &utf16("Domain"));
        push_av(&mut info, 1, &utf16("Server"));
        push_av(&mut info, AV_EOL, &[]);
        let mut temp = vec![1u8, 1, 0, 0, 0, 0, 0, 0];
        temp.extend([0u8; 8]);
        temp.extend(client);
        temp.extend([0u8; 4]);
        temp.extend(&info);
        temp.extend([0u8; 4]);
        let proof = hmac_md5(&ntowf, &[&SERVER_CHALLENGE, &temp]);
        assert_eq!(proof.to_vec(), hex("68 cd 0a b8 51 e5 1c 96 aa bc 92 7b eb ef 6a 1c"));
        assert_eq!(hmac_md5(&ntowf, &[&proof]).to_vec(), hex("8d e4 0c ca db c1 4a 82 f1 5c b0 ad 0d e9 5c a3"));
    }

    /// Обмен на записанном CHALLENGE: NEGOTIATE → CHALLENGE → AUTHENTICATE. Ответ NTLMv2 сходится с паролем, имя —
    /// как у SSPI Windows, в ответе — SPN, MIC и привязка к каналу (MD5 `gss_channel_bindings`).
    #[test]
    fn exchange_on_recorded_challenge() {
        let app = b"tls-server-end-point:0123456789abcdef0123456789abcdef";
        for (login, user, domain) in [(r"EXAMPLE\anna", "anna", "EXAMPLE"), ("anna@corp.example", "anna@corp.example", "")] {
            let mut ctx = Ntlm::new(login, "not-a-password", "HTTP/mail.example.test", Some(app)).unwrap();
            let first = ctx.step(None).unwrap();
            assert!(first.starts_with(b"NTLMSSP\0\x01\0\0\0") && first.len() == 40, "NEGOTIATE");
            assert!(!ctx.done);
            let auth = ctx.step(Some(&challenge())).unwrap();
            assert!(ctx.done);
            let (u, d, ok, pairs) = verify(&auth, "not-a-password");
            assert_eq!((u.as_str(), d.as_str()), (user, domain), "{login}");
            assert!(ok, "NTProofStr сходится с паролем ({login})");
            assert!(!verify(&auth, "other").2, "чужой пароль не сходится");
            let get = |id: u16| pairs.iter().find(|(i, _)| *i == id).map(|(_, v)| v.clone());
            assert_eq!(get(AV_CHANNEL_BINDINGS), Some(binding_hash(app).to_vec()), "MsvAvChannelBindings");
            assert_eq!(get(AV_TARGET_NAME), Some(utf16("HTTP/mail.example.test")), "MsvAvTargetName");
            assert_eq!(get(AV_FLAGS), Some(AV_FLAG_MIC.to_le_bytes().to_vec()), "MsvAvFlags: MIC");
            assert_eq!(get(AV_TIMESTAMP), Some(133_000_000_000_000_000u64.to_le_bytes().to_vec()), "время — сервера");
            assert_eq!(&auth[12..16], &[24, 0, 24, 0], "LMv2 — нули при отметке времени");
            let flags = u32::from_le_bytes(auth[60..64].try_into().unwrap());
            assert_eq!(flags & 0x4000_0000, 0, "без KEY_EXCH");
            assert_ne!(&auth[MIC_AT..MIC_AT + 16], &[0u8; 16], "MIC");
            assert!(ctx.step(Some(&challenge())).is_err(), "лишний шаг — ошибка");
        }
    }

    /// MIC — HMAC-MD5 ключа сеанса по трём сообщениям с нулевым MIC (так его проверяет сервер).
    #[test]
    fn mic_covers_all_messages() {
        let mut ctx = Ntlm::new(r"EXAMPLE\anna", "not-a-password", "HTTP/mail.example.test", None).unwrap();
        let negotiate = ctx.step(None).unwrap();
        let ch = challenge();
        let auth = ctx.authenticate(&ch, [7; 8], 0).unwrap();
        let nt = field(&auth, 20).unwrap();
        let ntowf = ntowf_v2(&md4::Md4::digest(utf16("not-a-password")).into(), "anna", "EXAMPLE");
        let session = hmac_md5(&ntowf, &[&nt[..16]]);
        let mut zeroed = auth.clone();
        zeroed[MIC_AT..MIC_AT + 16].fill(0);
        assert_eq!(&auth[MIC_AT..MIC_AT + 16], &hmac_md5(&session, &[&negotiate, &ch, &zeroed]));
    }

    /// Адрес почты отклонён — вторая попытка `ДОМЕН\имя` из CHALLENGE; у `DOMAIN\user` второй нет.
    #[test]
    fn alternative_login_from_challenge() {
        let mut ctx = Ntlm::new("anna.petrova@corp.example", "x", "HTTP/mail.corp.example", None).unwrap();
        assert_eq!(ctx.alternative(), None, "до CHALLENGE домена нет");
        ctx.step(None).unwrap();
        ctx.step(Some(&challenge())).unwrap();
        assert_eq!(ctx.alternative().as_deref(), Some(r"EXAMPLE\anna.petrova"));
        let mut ctx = Ntlm::new(r"CORP\anna", "x", "HTTP/mail.corp.example", None).unwrap();
        ctx.step(None).unwrap();
        ctx.step(Some(&challenge())).unwrap();
        assert_eq!(ctx.alternative(), None);
    }

    /// Без TLS — без привязки; мусор вместо CHALLENGE — ошибка, не паника; CHALLENGE без отметки времени — LMv2 и без
    /// MIC.
    #[test]
    fn edge_cases() {
        let mut ctx = Ntlm::new(r"EXAMPLE\anna", "not-a-password", "HTTP/mail.example.test", None).unwrap();
        ctx.step(None).unwrap();
        let (_, _, ok, pairs) = verify(&ctx.step(Some(&challenge())).unwrap(), "not-a-password");
        assert!(ok && pairs.iter().all(|(id, _)| *id != AV_CHANNEL_BINDINGS));
        for junk in [&b"garbage"[..], b"NTLMSSP\0\x02\0\0\0", b""] {
            let mut ctx = Ntlm::new(r"EXAMPLE\anna", "x", "HTTP/h", None).unwrap();
            ctx.step(None).unwrap();
            assert!(ctx.step(Some(junk)).is_err());
        }
        // Без TargetInfo: время клиента, LMv2 настоящий, MIC нет.
        let mut bare = challenge();
        bare[20..24].copy_from_slice(&(UNICODE | NTLM).to_le_bytes());
        let mut ctx = Ntlm::new(r"EXAMPLE\anna", "not-a-password", "HTTP/h", None).unwrap();
        ctx.step(None).unwrap();
        let auth = ctx.authenticate(&bare, [7; 8], 42).unwrap();
        let (_, _, ok, _) = verify(&auth, "not-a-password");
        assert!(ok);
        assert_ne!(field(&auth, 12).unwrap(), &[0u8; 24][..], "LMv2");
        assert_eq!(&auth[MIC_AT..MIC_AT + 16], &[0u8; 16], "без MIC");
    }
}
