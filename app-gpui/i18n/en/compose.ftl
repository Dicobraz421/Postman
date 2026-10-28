# Message: tab and reply in the reading pane (S3-01, S5-06b).
compose-title-new = New message
compose-send = Send
compose-attach = Attach
compose-to = To
compose-cc = Cc
compose-bcc = Bcc
compose-subject = Subject
compose-attachments = Attachments
compose-attachments-empty = No files — “Attach” or drop files onto the message.
compose-to-placeholder = Name or address — suggestions from the address book
compose-cc-placeholder = Cc
compose-bcc-placeholder = Bcc: recipients do not see each other
compose-subject-placeholder = Subject
compose-body-placeholder = Message text (Markdown: **bold**, *italic*, - list, > quote). Drop files onto the message to attach.
compose-size-b = { $n } B
compose-size-kb = { $n } KB
compose-size-mb = { $n } MB
compose-sending = Sending…
compose-saving = Saving the draft…
compose-autosaving = Autosaving…
compose-saved = Draft saved { $time }
compose-bad-addresses = Addresses not recognized: { $list }
compose-no-recipients = No recipients.
compose-blocked = Sending stopped: resolve the red “Before sending” item.
compose-no-server-send = Not sent: no connection to the server (demo mode). The message is kept as it was.
compose-no-server-save = Draft not saved: no connection to the server (demo mode). The text stays in the message.
compose-failed = Error: { $error }
compose-attach-failed = “{ $name }” not attached: { $error }
compose-large = Attachments: { $size } — the server may reject the message.
compose-confirm-close = Save the draft?
compose-confirm-yes = Yes
compose-confirm-no = No
compose-confirm-cancel = Cancel
# S/MIME signing (S5-12 phase 3).
compose-sign = Sign
compose-sign-on = The message will be signed with the From address certificate — turn off
compose-sign-off = Sign the message with the From address certificate
compose-sign-need-pin = Not signed: smart card PIN required · turn off "Sign" or insert the card and retry
compose-sign-no-card = Not signed: smart card not found · insert the card and retry
compose-sign-cancelled = Not signed: PIN entry cancelled
compose-sign-need-skzi = Not signed: GOST certificate needs a certified crypto provider (CryptoPro CSP or ViPNet CSP)
compose-sign-no-cert = Not signed: no signing certificate for { $address } · turn off "Sign"
compose-sign-eight-bit = Not signed: the message is not 7-bit
compose-sign-failed = Not signed: { $error }
# S/MIME encryption (S5-12 phase 4).
compose-seal-no-cert = Not sent: no encryption certificate for { $address }
compose-seal-no-own-cert = Not sent: you have no encryption certificate (the message is encrypted to you too)
compose-seal-need-skzi = Not sent: GOST recipients need a certified crypto provider (CryptoPro CSP or ViPNet CSP)
compose-seal-failed = Not encrypted: { $error }

# Forward as attachments (T1-17).
compose-attached-loading = Loading the messages to forward as attachments…
compose-attached-none = Could not forward: the messages are not on the server
compose-attached-skipped = { $n ->
    [one] { $n } message not attached: it is not on the server
   *[other] { $n } messages not attached: they are not on the server
}
