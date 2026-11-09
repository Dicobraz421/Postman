# Reading pane: attachments and invitations (S3-03).
reading-att-size = { $unit ->
    [b] { $n } B
    [kb] { $n } KB
   *[mb] { $n } MB
}
reading-saved = Saved: { $path }
reading-file-failed = “{ $name }”: { $error }
reading-blob-failed = Attachment not downloaded: { $error }
reading-blob-unavailable = the attachment is not available without a server

reading-invite-loading = 📅 Meeting invitation…
reading-invite-failed-parse = 📅 Invitation not read: { $error }
reading-invite-head = { $method ->
    [CANCEL] 📅 Meeting cancelled
    [REPLY] 📅 Invitation response
    [COUNTER] 📅 New time proposed
   *[other] 📅 Meeting invitation
}
reading-invite-when = When:
reading-invite-where = Where:
reading-invite-organizer = Organizer:
reading-invite-attendees = Attendees:
reading-invite-all-day = { $day }, all day
reading-invite-not-invited = You are not among the attendees — you cannot respond.
reading-invite-accept = ✔ Accept
reading-invite-tentative = ? Tentative
reading-invite-decline = ✖ Decline
reading-invite-sending = Sending the response…
reading-invite-mine = Your response: { $status }
reading-invite-sent-server = The response was sent through the calendar.
reading-invite-sent-mail = The response was sent to the organizer.
reading-invite-silent-saved = Saved to your calendar; no response was sent to the organizer.
reading-invite-silent = No response was sent to the organizer.
reading-threat = This message is marked as phishing. Don’t open its links or attachments unless you trust the sender.
reading-invite-sent-imported = The response was sent to the organizer, the meeting is added to the calendar.
reading-invite-failed = The response was not sent: { $error }
reading-partstat = { $s ->
    [accepted] accepted
    [tentative] tentative
    [declined] declined
    [delegated] delegated
   *[needs-action] no response
}
reading-weekday = { $d ->
    [0] Mon
    [1] Tue
    [2] Wed
    [3] Thu
    [4] Fri
    [5] Sat
   *[6] Sun
}
reading-link-mismatch = The link text says “{ $shown }”, but it leads to { $host }. Go to the real address?
reading-link-go = Go
reading-quick-reply = Reply to { $name }
reading-quick-reply-tip = Reply
