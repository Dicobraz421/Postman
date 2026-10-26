//! Ответ на приглашение по JMAP (S5-11): если событие уже есть в календаре на сервере (сервер разобрал письмо сам) —
//! меняем свой статус там, ответ организатору шлёт сервер; принятое/«под вопросом» письмом — в свой календарь без
//! рассылки. Письмо-ответ и состояние карточки — `postman_core::invite`.

use postman_calendar::{Invitation, Partstat, participant_id, partstat_json};
use postman_core::invite::import_event;
pub use postman_core::invite::reply_draft;
use postman_jmap::Client;

/// Попробовать ответить через календарь сервера. `None` — события там нет (или меня в нём нет).
pub async fn via_server(client: &Client, inv: &Invitation, me: &str, status: Partstat) -> postman_jmap::Result<Option<()>> {
    if !client.has(postman_jmap::CALENDARS) {
        return Ok(None);
    }
    let Some(ev) = client.event_by_uid(&inv.event.uid).await? else { return Ok(None) };
    let (Some(id), Some(pid)) = (ev["id"].as_str(), participant_id(&ev, me)) else { return Ok(None) };
    client.reply(id, &pid, partstat_json(status)).await?;
    Ok(Some(()))
}

/// Положить событие в первый видимый календарь (отказ не рассылается — его не кладём).
pub async fn import(client: &Client, inv: &Invitation, me: &str, status: Partstat) -> postman_jmap::Result<bool> {
    if status == Partstat::Declined || !client.has(postman_jmap::CALENDARS) {
        return Ok(false);
    }
    let mut cals = client.calendars().await?;
    cals.sort_by_key(|c| (!c.is_visible, c.sort_order));
    let Some(cal) = cals.first() else { return Ok(false) };
    client.create_event(import_event(inv, me, status, &cal.id), false).await?;
    Ok(true)
}

// --- состояние карточки приглашения ---
