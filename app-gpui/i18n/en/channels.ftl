# Conversation channels: Telegram in Settings → Accounts (CH-03).
tg-channels = Conversation channels
tg-channel = Telegram
tg-channel-hint = Telegram conversations next to your mail. The channel is in development and off by default.
tg-policy = Set by your organization's policy.
tg-add = Add Telegram account
tg-no-key = This build has no Telegram application key: sign-in is unavailable.
tg-sign-out = Sign out
tg-state-online = Online
tg-state-connecting = Connecting…
tg-state-offline = Offline, retrying in { $secs } s
tg-signin-title = Sign in to Telegram
tg-connecting = Connecting to Telegram…
tg-wait = Waiting for Telegram…
tg-phone = Phone number
tg-phone-hint = With the country code, for example +1 555 000 0000.
tg-get-code = Get code
tg-use-qr = Sign in with a QR code
tg-use-phone = Sign in with a phone number
tg-qr-hint = Open Telegram on your phone: Settings → Devices → Link Desktop Device, and point the camera at the code.
tg-code = { $via ->
    [app] Code from the Telegram app
    [sms] Code from the SMS
    [call] Code from the call
    [email] Code from the email
   *[other] Code
}
tg-code-length = Digits in the code: { $n }.
tg-resend = Send the code again
tg-sign-in = Sign in
tg-password = Two-step verification password
tg-password-hint = Hint: { $hint }
tg-err-phone = Telegram did not accept the number: check it and the country code.
tg-err-code = Wrong code: check it and try again.
tg-err-code-expired = The code has expired: send it again.
tg-err-password = Wrong two-step verification password.
tg-err-sign-up = This number is not registered with Telegram: sign up in the official app.
tg-err-denied = Telegram refused the sign-in.
tg-err-wait = Telegram asks to wait { $secs } s.
tg-err-network = No connection to Telegram: check the network and try again.
tg-err-signed-out = The sign-in has expired: sign in again.
tg-err-other = Could not sign in to Telegram.
