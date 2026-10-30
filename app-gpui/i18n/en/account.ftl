# Account and live mailbox (S3-04).
signin-url-placeholder = https://mail.example.com
signin-user-placeholder = user@example.com
signin-busy = Connecting…
signin-fill = Fill in the server address, login and password.
signin-failed = Sign-in failed: { $error }
account-sync-error = Sync: { $error }
account-sync-stopped = No connection to the server: sync stopped, working with the local copy.
account-remember-failed = Signed in, but the password was not saved: { $error }
# Adding an account in place and first run (S5-09).
signin-title = Welcome to Postman
signin-hint = Mail, calendar and contacts of a Stalwart server. Works offline with a local copy and sends nothing to third parties.
signin-new = New account
signin-new-hint = — set up in place, no window opens
signin-step-server = Server
signin-step-login = Login
signin-step-password = Password
signin-url = Server address
signin-url-hint = For example, mail.example.com — Postman finds the JMAP settings itself (/.well-known/jmap). Mail over IMAP: imaps://imap.example.com (SMTP is the same host or ?smtp=smtp.example.com; calendar and contacts — CalDAV and CardDAV of the same host, if it has them, or &dav=https://dav.example.com). Exchange: the mail domain or the EWS address and the login as an e-mail address — Postman finds EWS itself.
signin-user = Login
signin-user-hint = Usually your email address.
signin-password = Password
signin-store = Password storage
signin-store-windows = Keep in Windows Credential Manager
signin-store-never = Do not keep — ask at startup
signin-store-note = Postman itself never stores secrets — neither on disk nor in settings.
signin-tls = Connection: TLS 1.2 or 1.3, older versions are not supported. Windows checks the server certificate and its revocation.
signin-tls-insecure = The server certificate is not checked (POSTMAN_INSECURE_TLS — test stand only).
signin-change = Change
signin-next = Continue
signin-back = Back
signin-cancel = Cancel
signin-submit = Check and connect
signin-fill-url = Enter the server address.
signin-fill-user = Enter the login.
signin-bad-url = This does not look like a server address: mail.example.com or https://mail.example.com.
signin-demo = No server? Run with --demo · Ctrl+, — settings
# Windows account sign-in (T1-02b).
signin-windows = Sign in as the Windows user (no password)
signin-with-password = Sign in with a password
signin-windows-hint = Exchange in a domain: Postman signs in with the account you used to sign in to Windows. No password is asked for or stored.
signin-windows-failed = Couldn’t sign in with the Windows account — enter the password.
