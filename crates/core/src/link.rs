//! Связь с сервером (S5-08, DESIGN «Действия и состояния»): сервер не отвечает — синхронизация «без связи»,
//! повтор с растущей паузой, правки писем (метки, флаги, перемещения, удаление) копятся в очереди и уходят на
//! сервер, когда связь вернётся. Без I/O и таймеров — решения цикла синхронизации (`sync.rs`), проверяются тестами.

use std::collections::VecDeque;
use std::time::Duration;

/// Первая пауза перед повтором, секунды; дальше — вдвое, до [`MAX_RETRY`].
pub const FIRST_RETRY: u64 = 5;
/// Самая долгая пауза перед повтором, секунды.
pub const MAX_RETRY: u64 = 60;
/// Правок в очереди — не больше: дальше старые теряются (сервер всё равно вернёт письма такими, какие они у него).
pub const MAX_QUEUE: usize = 10_000;

/// Связь для интерфейса: строка состояния, метка у аккаунта, полоса над списком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Online,
    /// Сервер не отвечает: следующая попытка через `retry_in` секунд; правок ждёт отправки — `queued`.
    Offline {
        retry_in: u64,
        queued: usize,
    },
}

/// Пауза перед попыткой номер `attempt` (с нуля): 5, 10, 20, 40, 60, 60… с.
pub fn delay(attempt: u32) -> Duration {
    let secs = FIRST_RETRY.saturating_mul(1u64 << attempt.min(16)).min(MAX_RETRY);
    Duration::from_secs(secs)
}

/// Состояние связи и очередь правок `T` (команд синхронизации), сделанных без связи.
#[derive(Debug)]
pub struct Link<T> {
    down: bool,
    attempt: u32,
    queue: VecDeque<T>,
}

impl<T> Default for Link<T> {
    fn default() -> Self {
        Self { down: false, attempt: 0, queue: VecDeque::new() }
    }
}

impl<T> Link<T> {
    pub fn is_down(&self) -> bool {
        self.down
    }

    /// Попытка не удалась из-за сети: связи нет, пауза до следующей (каждая неудача — вдвое дольше).
    pub fn failed(&mut self) -> Duration {
        self.down = true;
        let wait = delay(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        wait
    }

    /// Сервер ответил. Связь только что вернулась — `Some` с правками из очереди (по порядку), иначе `None`.
    pub fn ok(&mut self) -> Option<Vec<T>> {
        if !self.down {
            return None;
        }
        self.down = false;
        self.attempt = 0;
        Some(self.queue.drain(..).collect())
    }

    /// Правка без связи — в очередь (переполнена — самая старая уходит).
    pub fn queue(&mut self, item: T) {
        if self.queue.len() >= MAX_QUEUE {
            self.queue.pop_front();
        }
        self.queue.push_back(item);
    }

    /// Вернуть неотправленный остаток в начало очереди (связь пропала посреди отправки очереди).
    pub fn requeue(&mut self, items: Vec<T>) {
        for item in items.into_iter().rev() {
            self.queue.push_front(item);
        }
        self.queue.truncate(MAX_QUEUE);
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Что показать: связь есть или до повтора `retry_in`.
    pub fn connection(&self, retry_in: Duration) -> Connection {
        if self.down { Connection::Offline { retry_in: retry_in.as_secs(), queued: self.queue.len() } } else { Connection::Online }
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, Link, MAX_QUEUE, delay};
    use std::time::Duration;

    #[test]
    fn retry_pause_doubles_up_to_a_minute() {
        let secs: Vec<u64> = (0..7).map(|a| delay(a).as_secs()).collect();
        assert_eq!(secs, [5, 10, 20, 40, 60, 60, 60]);
        assert_eq!(delay(u32::MAX).as_secs(), 60);
    }

    /// Офлайн: неудачи удлиняют паузу, правки копятся; ответ сервера отдаёт их по порядку и сбрасывает паузу.
    #[test]
    fn offline_queues_and_recovery_replays_in_order() {
        let mut l: Link<&str> = Link::default();
        assert_eq!(l.ok(), None, "связь и так есть — отдавать нечего");
        assert_eq!(l.connection(Duration::ZERO), Connection::Online);
        assert_eq!(l.failed(), Duration::from_secs(5));
        l.queue("flag");
        l.queue("move");
        assert_eq!(l.failed(), Duration::from_secs(10));
        assert!(l.is_down());
        assert_eq!(l.connection(Duration::from_secs(7)), Connection::Offline { retry_in: 7, queued: 2 });
        assert_eq!(l.ok(), Some(vec!["flag", "move"]));
        assert!(!l.is_down() && l.queued() == 0);
        assert_eq!(l.ok(), None);
        // После восстановления — снова с первой паузы.
        assert_eq!(l.failed(), Duration::from_secs(5));
    }

    /// Связь пропала посреди отправки очереди: остаток — снова в начало, перед правками, сделанными позже.
    #[test]
    fn unsent_rest_goes_back_to_the_front() {
        let mut l: Link<u32> = Link::default();
        l.failed();
        l.queue(3);
        l.requeue(vec![1, 2]);
        assert_eq!(l.ok(), Some(vec![1, 2, 3]));
    }

    #[test]
    fn queue_is_capped_dropping_the_oldest() {
        let mut l: Link<usize> = Link::default();
        l.failed();
        for i in 0..MAX_QUEUE + 3 {
            l.queue(i);
        }
        let q = l.ok().unwrap_or_default();
        assert_eq!(q.len(), MAX_QUEUE);
        assert_eq!(q.first(), Some(&3));
    }
}
