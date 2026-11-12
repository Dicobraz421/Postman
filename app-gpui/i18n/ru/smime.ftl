# Подпись S/MIME в области чтения (S5-12 Ф1): значок в шапке и карточка подробностей.
smime-checking = Проверка подписи…
smime-signed = Подписано
smime-signed-weak = Подписано · слабый алгоритм
smime-untrusted = Издатель не доверен
smime-mismatch = Адрес подписи не совпадает с «От»
smime-invalid = Подпись недействительна
smime-revoked = Сертификат отозван
smime-cert-bad = Сертификат подписи недействителен
smime-unverified = Подпись не проверена
smime-too-large = Подпись не проверена: письмо слишком большое
smime-details = Подробности подписи
smime-no-raw = сервер не отдал письмо целиком

smime-card-title = Цифровая подпись
smime-verdict-valid = Подпись верна, сертификат выпущен доверенным центром.
smime-verdict-untrusted = Подпись верна, но сертификат выпущен не доверенным центром.
smime-verdict-mismatch = Подпись верна, но адрес в сертификате ({ $cert }) не совпадает с адресом «От» ({ $from }).
smime-verdict-invalid = Содержимое письма изменено после подписи или подпись сделана другим ключом.
smime-verdict-expired = Срок действия сертификата подписи истёк или ещё не начался.
smime-verdict-usage = Сертификат не предназначен для подписи почты.
smime-verdict-distrusted = Сертификат в списке недоверенных Windows.
smime-verdict-revoked = Сертификат подписи отозван центром сертификации.
smime-verdict-rejected = Windows не подтвердила цепочку сертификатов: { $reason }.
smime-verdict-no-cert = В письме нет сертификата подписанта — подпись не проверить.
smime-verdict-unsupported = Алгоритм подписи не поддерживается: { $alg }.
smime-verdict-malformed = Подпись повреждена: { $error }.
smime-verdict-failed = Подпись не проверена: { $error }.
smime-verdict-too-large = Письмо больше 62 МБ — подпись не проверяется.
smime-verdict-not-signed = Подписи в письме не найдено.

smime-row-signer = Кто подписал
smime-row-issuer = Издатель
smime-row-valid = Срок действия
smime-row-fingerprint = Отпечаток SHA-256
smime-row-algorithm = Алгоритм
smime-row-engine = Проверено
smime-row-time = Время подписи
smime-row-revocation = Отзыв
smime-corporate = корпоративный
smime-valid-range = { $from } — { $to }

smime-engine-postman = Postman, без СКЗИ (реализация не сертифицирована)
smime-engine-skzi = СКЗИ ({ $name }): цепочка и отзыв; подпись — Postman

smime-rev-good = не отозван · проверено { $at }
smime-rev-revoked = отозван
smime-rev-unknown = не проверен — нет связи с центром сертификации
smime-rev-gost = не проверен (ГОСТ без СКЗИ)
smime-rev-not-checked = не проверялся
smime-rev-demo = не проверялся (демо-ящик)

smime-reason-untrusted = корневой сертификат не доверен
smime-reason-distrusted = сертификат в списке недоверенных
smime-reason-expired = срок действия сертификата
smime-reason-usage = сертификат не для защиты почты
smime-reason-signature = подпись сертификата неверна
smime-reason-failed = ошибка проверки цепочки

smime-saved = Сертификат сохранён для шифрования писем этому адресату.
smime-weak-note = Слабый алгоритм (SHA-1 или ключ RSA короче 2048 бит): такую подпись можно подделать.
smime-key-rsa = RSA { $bits }
smime-key-ec = ECDSA { $curve }
smime-key-gost = ГОСТ Р 34.10-2012 ({ $bits } бит, { $params })
smime-hash-streebog = Стрибог-{ $bits }
# Шифрованные письма (S5-12 Ф2).
smime-encrypted = Зашифровано
smime-row-encryption = Шифрование
smime-verdict-encrypted = Письмо зашифровано, подписи внутри нет.
smime-list-preview = Зашифрованное письмо
smime-enc-opening = Расшифровка…
smime-enc-need-pin = Нужен PIN смарт-карты
smime-enc-no-card = Смарт-карта не вставлена
smime-enc-cancelled = Ввод PIN отменён
smime-enc-enter = Ввести
smime-enc-retry = Повторить
smime-enc-not-for-you = Письмо зашифровано не для вас: нет ключа к сертификату { $who }
smime-enc-damaged = Письмо повреждено: проверка целостности не прошла
smime-enc-unsupported = Алгоритм не поддерживается: { $alg }
smime-enc-nested = Шифрование внутри шифрования не открывается
smime-enc-too-large = Письмо слишком большое для безопасного открытия
smime-enc-failed = Письмо не расшифровано: { $error }
smime-enc-part-open = Расшифрованное вложение не открывается из временной папки — сохраните его («Сохранить как»).
smime-enc-part-gone = Письмо закрыто — откройте его снова
smime-enc-need-skzi = Письмо зашифровано по ГОСТ: нужно сертифицированное СКЗИ (КриптоПро CSP или ViPNet CSP)
