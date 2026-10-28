# Selection and bulk actions (S5-08): checkboxes, the bar above the list, the summary, the context menu, phishing.
bulk-pick = Select (Ctrl+click; Shift+click — a range)
bulk-select-all = Select all
bulk-selected = Selected: { $n }
bulk-of = of { $total }
bulk-clear-hint = Clear selection
bulk-read = Read
bulk-unread = Unread
bulk-flag = Flag
bulk-unflag = Unflag
bulk-archive = Archive
bulk-forward-attached = Forward
bulk-delete = Delete
bulk-uncategorize = Clear category
bulk-move-plain = Move
bulk-category-plain = Category
bulk-due-plain = Due date
bulk-phishing = Report phishing
bulk-letters = { $n ->
    [one] { $n } message
   *[other] { $n } messages
}
bulk-read-only = Not changed: the shared mailbox folder is read-only.
bulk-nothing = Nothing to change: the messages are already like that.
bulk-skipped = { $n ->
    [one] { $n } message skipped: no rights in the shared mailbox or another account.
   *[other] { $n } messages skipped: no rights in the shared mailbox or another account.
}
bulk-undo = Undo
bulk-undone = Undone · messages are back
bulk-done-read = { $n ->
    [one] Read · { $n } message
   *[other] Read · { $n } messages
}
bulk-done-unread = { $n ->
    [one] Unread · { $n } message
   *[other] Unread · { $n } messages
}
bulk-done-flag = { $n ->
    [one] Flagged · { $n } message
   *[other] Flagged · { $n } messages
}
bulk-done-unflag = { $n ->
    [one] Unflagged · { $n } message
   *[other] Unflagged · { $n } messages
}
bulk-done-category = { $n ->
    [one] Category “{ $name }” · { $n } message
   *[other] Category “{ $name }” · { $n } messages
}
bulk-done-uncategorize = { $n ->
    [one] Categories cleared · { $n } message
   *[other] Categories cleared · { $n } messages
}
bulk-done-move = { $n ->
    [one] Moved to “{ $folder }” · { $n } message
   *[other] Moved to “{ $folder }” · { $n } messages
}
bulk-done-archive = { $n ->
    [one] Moved to Archive · { $n } message
   *[other] Moved to Archive · { $n } messages
}
bulk-done-trash = { $n ->
    [one] Moved to Deleted Items · { $n } message
   *[other] Moved to Deleted Items · { $n } messages
}
bulk-done-phishing = { $n ->
    [one] Reported as phishing · { $n } message in Junk
   *[other] Reported as phishing · { $n } messages in Junk
}
bulk-done-phishing-marked = { $n ->
    [one] Reported as phishing · { $n } message marked as a threat
   *[other] Reported as phishing · { $n } messages marked as a threat
}
bulk-done-purge = { $n ->
    [one] Deleted permanently · { $n } message
   *[other] Deleted permanently · { $n } messages
}
bulk-purge-confirm = { $n ->
    [one] Delete { $n } message in Deleted Items permanently?
   *[other] Delete { $n } messages in Deleted Items permanently?
}
bulk-phishing-subject = { $n ->
    [one] Phishing: { $n } message
   *[other] Phishing: { $n } messages
}
bulk-phishing-text = A user reported phishing. The messages are attached (.eml) as received.
bulk-phishing-sent = Phishing report sent to { $to }
bulk-phishing-failed = Phishing report not sent: { $error }
bulk-size-kb = { $n } KB
bulk-size-mb = { $n } MB
bulk-senders-more = { $who } and { $n } more
bulk-summary-title = { $n ->
    [one] { $n } message selected
   *[other] { $n } messages selected
}
bulk-summary-hint = Actions apply to all selected
bulk-summary-from = From: { $who }
bulk-summary-unread = unread: { $n }
bulk-summary-more = and { $n } more
bulk-unpick = Remove from selection
bulk-summary-clear = Clear

# Message context menu.
menu-reply = Reply
menu-reply-all = Reply all
menu-forward = Forward
menu-forward-attached = Forward as attachments
menu-read = Mark as read
menu-unread = Mark as unread
menu-flag = Flag
menu-unflag = Unflag
menu-category = Category
menu-due = Due date…
menu-remind = Remind me…
menu-wait = Wait for reply…
menu-expire = Expires…
menu-move = Move
menu-archive = Archive
menu-delete = Delete
menu-phishing = Report phishing
menu-on-selection = { $n ->
    [one] { $n } message selected
   *[other] { $n } messages selected
}
