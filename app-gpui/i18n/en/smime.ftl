# S/MIME signature in the reading pane (S5-12 phase 1): header badge and details card.
smime-checking = Checking signature…
smime-signed = Signed
smime-signed-weak = Signed · weak algorithm
smime-untrusted = Issuer not trusted
smime-mismatch = Signature address differs from “From”
smime-invalid = Invalid signature
smime-revoked = Certificate revoked
smime-cert-bad = Signing certificate is not valid
smime-unverified = Signature not verified
smime-too-large = Signature not verified: message too large
smime-details = Signature details
smime-no-raw = the server did not return the full message

smime-card-title = Digital signature
smime-verdict-valid = The signature is valid; the certificate was issued by a trusted authority.
smime-verdict-untrusted = The signature is valid, but the certificate was issued by an untrusted authority.
smime-verdict-mismatch = The signature is valid, but the certificate address ({ $cert }) differs from the “From” address ({ $from }).
smime-verdict-invalid = The message was changed after signing, or it was signed with another key.
smime-verdict-expired = The signing certificate has expired or is not valid yet.
smime-verdict-usage = The certificate is not meant for signing mail.
smime-verdict-distrusted = The certificate is in the Windows untrusted list.
smime-verdict-revoked = The signing certificate was revoked by the certification authority.
smime-verdict-rejected = Windows did not confirm the certificate chain: { $reason }.
smime-verdict-no-cert = The message has no signer certificate, so the signature cannot be verified.
smime-verdict-unsupported = Unsupported signature algorithm: { $alg }.
smime-verdict-malformed = The signature is damaged: { $error }.
smime-verdict-failed = Signature not verified: { $error }.
smime-verdict-too-large = The message is larger than 62 MB; its signature is not verified.
smime-verdict-not-signed = No signature found in the message.

smime-row-signer = Signed by
smime-row-issuer = Issuer
smime-row-valid = Valid
smime-row-fingerprint = SHA-256 fingerprint
smime-row-algorithm = Algorithm
smime-row-engine = Verified by
smime-row-time = Signing time
smime-row-revocation = Revocation
smime-corporate = corporate
smime-valid-range = { $from } – { $to }

smime-engine-postman = Postman, no certified CSP (implementation not certified)
smime-engine-skzi = Certified CSP ({ $name }): chain and revocation; signature — Postman

smime-rev-good = not revoked · checked { $at }
smime-rev-revoked = revoked
smime-rev-unknown = not checked — the certification authority is unreachable
smime-rev-gost = not checked (GOST without a certified CSP)
smime-rev-not-checked = not checked
smime-rev-demo = not checked (demo mailbox)

smime-reason-untrusted = the root certificate is not trusted
smime-reason-distrusted = the certificate is in the untrusted list
smime-reason-expired = certificate validity period
smime-reason-usage = the certificate is not for mail protection
smime-reason-signature = the certificate signature is invalid
smime-reason-failed = chain check failed

smime-saved = Certificate saved for encrypting mail to this sender.
smime-weak-note = Weak algorithm (SHA-1 or an RSA key shorter than 2048 bits): such a signature can be forged.
smime-key-rsa = RSA { $bits }
smime-key-ec = ECDSA { $curve }
smime-key-gost = GOST R 34.10-2012 ({ $bits } bit, { $params })
smime-hash-streebog = Streebog-{ $bits }
# Encrypted messages (S5-12 phase 2).
smime-encrypted = Encrypted
smime-row-encryption = Encryption
smime-verdict-encrypted = The message is encrypted; there's no signature inside.
smime-list-preview = Encrypted message
smime-enc-opening = Decrypting…
smime-enc-need-pin = Smart card PIN required
smime-enc-no-card = Smart card is not inserted
smime-enc-cancelled = PIN entry cancelled
smime-enc-enter = Enter
smime-enc-retry = Retry
smime-enc-not-for-you = The message isn't encrypted for you: no key for certificate { $who }
smime-enc-damaged = The message is damaged: integrity check failed
smime-enc-unsupported = Algorithm not supported: { $alg }
smime-enc-nested = Encryption inside encryption is not opened
smime-enc-too-large = The message is too large to open safely
smime-enc-failed = The message is not decrypted: { $error }
smime-enc-part-open = Decrypted attachments are not opened from a temp folder — save it ("Save as").
smime-enc-part-gone = The message was closed — open it again
smime-enc-need-skzi = The message is encrypted with GOST: a certified crypto provider (CryptoPro CSP or ViPNet CSP) is required
