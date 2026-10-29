//! Корпус «только новый текст» (S5-05): синтетические письма популярных клиентов. В каждом случае — что остаётся
//! в новом тексте (`kept`) и что уходит в историю (`stripped`: есть в полном IR, нет в новом тексте).
//! Точность важнее полноты: случаи без истории (`stripped` пуст) проверяют, что текст автора не теряется.
//! Имена и адреса — выдуманные.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест

use postman_ir::{Block, Document, from_html, from_plain, markdown_to_html, plain_text};

struct Case {
    name: &'static str,
    html: bool,
    source: &'static str,
    kept: &'static [&'static str],
    stripped: &'static [&'static str],
}

const fn html(name: &'static str, source: &'static str, kept: &'static [&'static str], stripped: &'static [&'static str]) -> Case {
    Case { name, html: true, source, kept, stripped }
}

const fn text(name: &'static str, source: &'static str, kept: &'static [&'static str], stripped: &'static [&'static str]) -> Case {
    Case { name, html: false, source, kept, stripped }
}

const CORPUS: &[Case] = &[
    // --- Outlook ---
    html(
        "outlook-ru-desktop",
        r##"<html><body><div class=WordSection1><p class=MsoNormal>Добрый день!<o:p></o:p></p>
<p class=MsoNormal>Согласовано, отправляю договор.<o:p></o:p></p><p class=MsoNormal>&nbsp;</p>
<p class=MsoNormal>С уважением,<br>Анна Смирнова</p>
<div><div style='border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0cm 0cm 0cm'>
<p class=MsoNormal><b>От:</b> Петров Иван &lt;i.petrov@example.ru&gt; <br><b>Отправлено:</b> 20 октября 2026 г. 10:15<br>
<b>Кому:</b> Смирнова Анна &lt;a.smirnova@example.ru&gt;<br><b>Тема:</b> Договор</p></div></div>
<p class=MsoNormal>Анна, пришлите, пожалуйста, договор.</p></div></body></html>"##,
        &["Согласовано, отправляю договор.", "С уважением,", "Анна Смирнова"],
        &["Отправлено:", "пришлите, пожалуйста"],
    ),
    html(
        "outlook-en-web",
        r##"<html><body><div dir="ltr">Thanks, I will check it today.</div><div dir="ltr">Anna</div>
<div id="appendonsend"></div><hr style="display:inline-block;width:98%" tabindex="-1">
<div id="divRplyFwdMsg" dir="ltr"><font face="Calibri, sans-serif" style="font-size:11pt" color="#000000">
<b>From:</b> Ivan Petrov &lt;i.petrov@example.com&gt;<br><b>Sent:</b> Monday, October 20, 2026 10:15 AM<br>
<b>To:</b> Anna Smith &lt;a.smith@example.com&gt;<br><b>Subject:</b> Report</font><div>&nbsp;</div></div>
<div>Anna, please review the report.</div></body></html>"##,
        &["Thanks, I will check it today.", "Anna"],
        &["Sent:", "please review the report"],
    ),
    html(
        "outlook-en-mac",
        r##"<html><body><p>Done, see the attachment.</p>
<div style="border:none;border-top:solid #B5C4DF 1.0pt;padding:3.0pt 0cm 0cm 0cm">
<p><b>From: </b>Ivan Petrov &lt;i.petrov@example.com&gt;<br><b>Date: </b>Monday, 20 October 2026 at 10:15<br>
<b>To: </b>Anna Smith<br><b>Subject: </b>Plan</p></div><p>Could you send the plan?</p></body></html>"##,
        &["Done, see the attachment."],
        &["Date:", "Could you send the plan?"],
    ),
    text(
        "outlook-en-plain",
        "Thanks, will do.\n\nAnna\n\n-----Original Message-----\nFrom: Ivan Petrov <i.petrov@example.com>\nSent: Monday, October 20, 2026 10:15 AM\nTo: Anna Smith <a.smith@example.com>\nSubject: Report\n\nPlease send the report.",
        &["Thanks, will do.", "Anna"],
        &["Original Message", "Please send the report."],
    ),
    text(
        "outlook-ru-plain",
        "Принято.\n\n________________________________\nОт: Иван Петров <i.petrov@example.ru>\nОтправлено: 20 октября 2026 г. 10:15\nКому: Анна Смирнова\nТема: Отчёт\n\nПришлите отчёт.",
        &["Принято."],
        &["________", "Отправлено:", "Пришлите отчёт."],
    ),
    text(
        // Шапка без пустой строки над ней: абзац разрезается.
        "outlook-ru-plain-inline-header",
        "Ок, сделаю.\nАнна\nОт: Иван Петров\nОтправлено: 20 октября 2026 г. 10:15\nКому: Анна\nТема: Задача\n\nСделайте задачу.",
        &["Ок, сделаю.", "Анна"],
        &["Отправлено:", "Сделайте задачу."],
    ),
    // --- Gmail ---
    html(
        "gmail-en",
        r##"<div dir="ltr">Sounds good, see you on Monday.</div><br><div class="gmail_quote gmail_quote_container">
<div dir="ltr" class="gmail_attr">On Mon, Oct 20, 2026 at 10:15 AM Ivan Petrov &lt;<a href="mailto:i.petrov@example.com">i.petrov@example.com</a>&gt; wrote:<br></div>
<blockquote class="gmail_quote" style="margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex">
<div dir="ltr">Shall we meet on Monday?</div></blockquote></div>"##,
        &["Sounds good, see you on Monday."],
        &["wrote:", "Shall we meet on Monday?"],
    ),
    html(
        "gmail-ru",
        r##"<div dir="ltr">Да, подходит.</div><br><div class="gmail_quote"><div dir="ltr" class="gmail_attr">
пн, 20 окт. 2026 г. в 10:15, Иван Петров &lt;i.petrov@example.ru&gt;:<br></div>
<blockquote class="gmail_quote"><div dir="ltr">Встречаемся в понедельник?</div></blockquote></div>"##,
        &["Да, подходит."],
        &["окт. 2026", "Встречаемся в понедельник?"],
    ),
    html(
        // Пересылка у Gmail — тоже `gmail_quote`, но это содержимое письма.
        "gmail-forward",
        r##"<div dir="ltr">FYI</div><br><div class="gmail_quote"><div dir="ltr" class="gmail_attr">---------- Forwarded message ---------<br>
From: <strong>Ivan Petrov</strong> &lt;i.petrov@example.com&gt;<br>Date: Mon, Oct 20, 2026 at 10:15 AM<br>Subject: Budget<br>To: Anna</div><br>
<div dir="ltr">The budget is approved.</div></div>"##,
        &["FYI", "The budget is approved."],
        &[],
    ),
    // --- Apple Mail ---
    html(
        "apple-mail-en",
        r##"<html><body><div>Sure, I will be there.</div><div><br><blockquote type="cite"><div>On Oct 20, 2026, at 10:15, Ivan Petrov &lt;i.petrov@example.com&gt; wrote:</div><br class="Apple-interchange-newline">
<div><div>Are you coming tomorrow?</div></div></blockquote></div></body></html>"##,
        &["Sure, I will be there."],
        &["wrote:", "Are you coming tomorrow?"],
    ),
    html(
        "apple-mail-ru",
        r##"<html><body><div>Спасибо!</div><div><br><div>20 окт. 2026 г., в 10:15, Иван Петров &lt;i.petrov@example.ru&gt; написал(а):</div><br>
<blockquote type="cite"><div>Высылаю презентацию.</div></blockquote></div></body></html>"##,
        &["Спасибо!"],
        &["написал(а):", "Высылаю презентацию."],
    ),
    html(
        // Ответы между цитатами: уходит только последняя цитата, ответы автора остаются.
        "apple-mail-interleaved",
        r##"<html><body><blockquote type="cite"><div>When is the release?</div></blockquote><div>On Friday.</div>
<blockquote type="cite"><div>Thanks, Ivan</div></blockquote></body></html>"##,
        &["When is the release?", "On Friday."],
        &["Thanks, Ivan"],
    ),
    // --- Thunderbird ---
    html(
        "thunderbird-en",
        r##"<html><body><p>Agreed.</p><div class="moz-cite-prefix">On 20.10.2026 10:15, Ivan Petrov wrote:<br></div>
<blockquote type="cite" cite="mid:abc@example.com"><p>Let us move the call to 11:00.</p></blockquote></body></html>"##,
        &["Agreed."],
        &["wrote:", "move the call"],
    ),
    html(
        // Ответ снизу: текст автора после цитаты — история не отделяется.
        "thunderbird-bottom-post",
        r##"<html><body><div class="moz-cite-prefix">On 20.10.2026 10:15, Ivan Petrov wrote:<br></div>
<blockquote type="cite"><p>Which version?</p></blockquote><p>Version 2, as discussed.</p></body></html>"##,
        &["Which version?", "Version 2, as discussed."],
        &[],
    ),
    text(
        "thunderbird-ru-plain",
        "Согласен.\n\n20.10.2026 10:15, Иван Петров пишет:\n> Переносим созвон на 11:00.\n>\n> Иван",
        &["Согласен."],
        &["пишет:", "Переносим созвон"],
    ),
    // --- Обычный текст ---
    text(
        "gmail-plain",
        "Ok.\n\nOn Mon, Oct 20, 2026 at 10:15 AM Ivan Petrov <i.petrov@example.com> wrote:\n\n> Is the build green?\n> Ivan",
        &["Ok."],
        &["wrote:", "Is the build green?"],
    ),
    text(
        // Длинная строка подписи цитаты перенесена клиентом.
        "gmail-plain-wrapped-attribution",
        "Ok.\n\nOn Mon, Oct 20, 2026 at 10:15 AM Ivan Petrov <\ni.petrov@example.com> wrote:\n\n> Old text",
        &["Ok."],
        &["Ivan Petrov", "Old text"],
    ),
    text(
        // Подпись цитаты без пустой строки после текста автора: абзац разрезается, «Thanks!» остаётся.
        "attribution-in-author-paragraph",
        "Thanks!
On Mon, Oct 20, 2026 at 10:15 AM Ivan Petrov <i.petrov@example.com> wrote:
> Old text",
        &["Thanks!"],
        &["wrote:", "Old text"],
    ),
    text(
        "yandex-plain",
        "Спасибо!\n\n20.10.2026, 10:15, \"Иван Петров\" <i.petrov@example.ru>:\n> Счёт во вложении.",
        &["Спасибо!"],
        &["Счёт во вложении."],
    ),
    text(
        "nested-chain-plain",
        "Новый ответ.\n\n20.10.2026 12:00, Анна пишет:\n> Средний ответ.\n>\n> 20.10.2026 10:15, Иван пишет:\n>> Самое старое.",
        &["Новый ответ."],
        &["Средний ответ.", "Самое старое."],
    ),
    // --- Точность: текст автора не трогаем ---
    text(
        // Цитата без строки «… пишет:» — автор привёл её сам.
        "own-quote-without-attribution",
        "Как сказано в регламенте:\n\n> Пункт 3.2: срок — пять дней.",
        &["Как сказано в регламенте:", "срок — пять дней"],
        &[],
    ),
    text(
        "wrote-without-date",
        "Here is what the customer wrote:\n\n> We need it by Friday.",
        &["Here is what the customer wrote:", "We need it by Friday."],
        &[],
    ),
    text(
        "from-word-in-text",
        "From: the start of the project we agreed on weekly calls.\nTo be honest, it works.",
        &["From: the start of the project", "To be honest"],
        &[],
    ),
    html(
        // «Ответы ниже»: текст автора — внутри старого письма.
        "outlook-inline-answers",
        r##"<html><body><p>Ответы ниже красным.</p><div style='border:none;border-top:solid #E1E1E1 1.0pt'>
<p><b>От:</b> Иван Петров<br><b>Отправлено:</b> 20 октября 2026 г. 10:15<br><b>Кому:</b> Анна<br><b>Тема:</b> Вопросы</p></div>
<p>1. Срок? — <span style="color:red">До пятницы.</span></p></body></html>"##,
        &["Ответы ниже красным.", "До пятницы."],
        &[],
    ),
    text(
        // Только цитата: нового текста нет — письмо целиком.
        "quote-only",
        "On Mon, Oct 20, 2026 at 10:15 AM Ivan Petrov <i.petrov@example.com> wrote:\n> Hello",
        &["Hello"],
        &[],
    ),
    text(
        "forward-plain",
        "Посмотри.\n\n-------- Пересланное сообщение --------\nОт: Иван Петров\nДата: 20.10.2026 10:15\nКому: Анна\nТема: Смета\n\nСмета готова.",
        &["Посмотри.", "Смета готова."],
        &[],
    ),
];

/// Текст блоков без разметки (цитаты, списки, таблицы — вглубь).
fn md(blocks: &[Block]) -> String {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => out.push(plain_text(content)),
            Block::Quote(inner) => out.push(md(inner)),
            Block::List { items, .. } => out.extend(items.iter().map(|i| md(i))),
            Block::Code(s) => out.push(s.clone()),
            Block::Table(rows) => out.extend(rows.iter().flatten().map(|c| plain_text(c))),
            Block::Rule => out.push("---".into()),
            Block::Figure => {}
        }
    }
    out.join(
        "
",
    )
}

fn parse(c: &Case) -> Document {
    if c.html { from_html(c.source) } else { from_plain(c.source) }
}

#[test]
fn corpus_keeps_new_text_and_strips_history() {
    let mut failures = Vec::new();
    for c in CORPUS {
        let doc = parse(c);
        let (fresh, full) = (md(doc.new_text()), md(&doc.blocks));
        for k in c.kept {
            if !fresh.contains(k) {
                failures.push(format!("{}: потерян текст «{k}»\n--- новый текст:\n{fresh}", c.name));
            }
        }
        for s in c.stripped {
            if fresh.contains(s) {
                failures.push(format!("{}: не убрано «{s}»\n--- новый текст:\n{fresh}", c.name));
            }
            if !full.contains(s) {
                failures.push(format!("{}: «{s}» нет и в полном IR\n--- полный:\n{full}", c.name));
            }
        }
        // Без истории — письмо целиком.
        if c.stripped.is_empty() && doc.history.is_some() {
            failures.push(format!("{}: история отделена, хотя не должна (блок {:?})\n{full}", c.name, doc.history));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Свой ответ из композера Postman: Markdown с цитатой `> ` и строкой «…, … пишет:» → HTML → IR.
#[test]
fn postman_reply_is_stripped() {
    let md_reply = "Готово, проверьте.\n\nС уважением, Анна\n\n20.10.2026 10:15, Иван Петров пишет:\n> Проверьте, пожалуйста, расчёт.\n";
    let doc = from_html(&markdown_to_html(md_reply));
    let fresh = md(doc.new_text());
    assert!(fresh.contains("Готово, проверьте.") && fresh.contains("С уважением, Анна"), "{fresh}");
    assert!(!fresh.contains("пишет:") && !fresh.contains("расчёт"), "{fresh}");
}

/// Граница истории переживает предупреждение над письмом и сериализацию (ответ воркера).
#[test]
fn history_survives_prepend_and_serde() {
    let mut doc = from_plain("Ok.\n\nOn Mon, Oct 20, 2026 at 10:15 AM Ivan <i@example.com> wrote:\n> old");
    assert_eq!(doc.history, Some(1));
    doc.prepend(Block::Rule);
    assert_eq!(doc.history, Some(2));
    assert_eq!(doc.new_text().len(), 2);
    let json = serde_json::to_string(&doc).unwrap();
    let back: Document = serde_json::from_str(&json).unwrap();
    assert_eq!(back, doc);
    // Ответ прежнего воркера — без поля: истории нет.
    let old: Document = serde_json::from_str(r##"{"blocks":[],"remote_images_blocked":0}"##).unwrap();
    assert_eq!(old.history, None);
}
