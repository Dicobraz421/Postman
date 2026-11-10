# Message as a tab, reply in the reading pane, “Before sending” (S5-06b).
compose-external = External
compose-expand = Pop out
compose-fmt-bold = B
compose-fmt-italic = I
compose-fmt-underline = U
compose-template = Template
compose-template-empty = No templates yet: set them up in Settings → Signatures and templates.
compose-template-hint = In the message text: / and the template command, then a space.
compose-template-fields = Template fields “{ $name }”:
compose-template-insert = Insert
compose-template-raw = Without values
compose-template-cancel = Cancel
compose-signature = Signature: { $name }
compose-signature-none = none
compose-mark-none = Label: none
compose-mark-confidential = Label: Confidential
compose-mark-hint = Recipients see the label in Postman; there is no encryption yet.
compose-wait = Await reply
compose-wait-days = { $n ->
    [one] { $n } day
   *[other] { $n } days
}
compose-wait-days-hint = How long to wait for a reply — click to change
compose-category = Category
compose-category-none = No category
compose-delete-confirm = Delete the draft?
compose-undo-hint = Undo send { $s } s
compose-undo-suffix = · undo send { $s } s
checks-title = Before sending
checks-count = { $n ->
    [one] { $n } item
   *[other] { $n } items
}
checks-none = nothing found
checks-clear = Nothing to check.
checks-card = The text looks like a payment card number
checks-card-detail = Payment data leak: sending is stopped until the number is hidden.
checks-card-external = Payment data leak. The message goes to external recipients.
checks-password = The text looks like a password
checks-token = The text looks like an access token
checks-private-key = The text contains a private key
checks-secret-detail = Every recipient keeps the secret: sending is stopped until it is hidden.
checks-mask = Mask
checks-not-card = Not a card
checks-not-secret = Not a secret
checks-attachment = An attachment is mentioned (“{ $word }”), but no file is attached
checks-attachment-detail = The message has no attached files.
checks-attachment-card = An attachment is mentioned — no file attached
checks-attach = Attach a file
checks-ignore = Ignore
checks-external = External recipients: { $n } ({ $domains })
checks-external-detail = "Confidential": the message leaves the company — encrypted with the recipients' certificates.
checks-review = Review recipients
checks-ok = OK
checks-moved = { $name } last wrote from a different address
checks-moved-detail = { $from } → { $to } ({ $when })
checks-replace = Replace the address
checks-keep = Keep
checks-today = today
checks-days-ago = { $n ->
    [one] { $n } day ago
   *[other] { $n } days ago
}
checks-no-cert = No certificate: { $names }
checks-no-cert-detail = "Confidential" means encrypted; without a certificate the recipient can't read it. It can't be sent while they're in the message.
checks-no-cert-remove = Remove them
checks-no-cert-unmark = Remove "Confidential"
checks-no-cert-unmark-confirm = The message goes unencrypted · Yes, remove
checks-split = Two messages will be sent: separately — { $names } (GOST)
checks-split-detail = These recipients have GOST certificates, the others RSA or EC. Both messages go to Sent, both encrypted to you as well.
checks-no-cert-find = Find in directory
checks-no-cert-searching = Looking for certificates in the directory…
checks-no-cert-not-in-directory = No certificate in the directory either. Without it the recipient cannot read the message — remove them or turn off Confidential.
checks-no-cert-refused = The certificate in the directory was not accepted: it has expired or cannot be trusted. Without it the recipient cannot read the message — remove them or turn off Confidential.
checks-no-cert-directory-failed = { $reason } Remove the recipients or turn off Confidential.
