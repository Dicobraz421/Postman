# Message lifecycle (S5-04): deadline labels, picker, reminders, settings.
# Row labels: sentence case (DESIGN «Строка письма и метки»).
lifecycle-due = Due
lifecycle-due-by = Due { $when }
lifecycle-due-today = Today
lifecycle-tomorrow-short = tomorrow
lifecycle-overdue-days = Overdue { $n } d
lifecycle-overdue-hours = Overdue { $n } h
lifecycle-waiting = Waiting for reply
lifecycle-waiting-today = Waiting for reply · today
lifecycle-waiting-days = Waiting for reply · { $n } d
lifecycle-no-reply = No reply · { $n } d
lifecycle-remind-at = Remind { $when }
lifecycle-expires = Expires
lifecycle-expires-days = Expires in { $n } d
lifecycle-expires-hours = Expires in { $n } h
lifecycle-archives = To archive
lifecycle-archives-days = To archive in { $n } d
lifecycle-archives-hours = To archive in { $n } h
# Message header buttons.
lifecycle-act-due = Due date
lifecycle-act-remind = Remind me
lifecycle-act-wait = Wait for reply
lifecycle-act-expire = Expires
# Picker.
lifecycle-pick-due = Due date
lifecycle-pick-remind = Remind me
lifecycle-pick-wait = Wait for reply
lifecycle-pick-expire = Expires
lifecycle-today = Today
lifecycle-tomorrow = Tomorrow
lifecycle-friday = On Friday
lifecycle-next-week = Next week
lifecycle-later-today = Later today
lifecycle-monday = On Monday
lifecycle-days = In { $n } d
lifecycle-in-days = In { $n } d
lifecycle-pick-date = Pick a date…
lifecycle-pick-date-time = Pick a date and time…
lifecycle-date-placeholder = DD.MM.YYYY
lifecycle-date-time-placeholder = DD.MM.YYYY HH:MM
lifecycle-set = Set
lifecycle-bad-date = Can't read it: date DD.MM.YYYY, time HH:MM.
lifecycle-past = This time has already passed.
lifecycle-remove-due = Remove due date
lifecycle-remove-remind = Don't remind
lifecycle-remove-wait = Stop waiting
lifecycle-never = Never
lifecycle-read-only = This shared folder is read-only: no due dates here.
lifecycle-snoozed = I'll remind you { $when }.
# Reminders and toasts.
lifecycle-reminder = Reminder
lifecycle-open = Open
lifecycle-dismiss = Dismiss
lifecycle-no-reply-toast = No reply: “{ $subject }” is back in the Inbox.
lifecycle-replied-toast = Reply received: “{ $subject }”.
lifecycle-expired-toast = Messages expired: { $n }.
lifecycle-newsletters-toast = Newsletters archived: { $n }.
# Group on top of the list: overdue and fired reminders.
lifecycle-group-top = Needs attention
# Name of the Archive folder when the mailbox has none (created with role archive).
lifecycle-archive-folder = Archive
# Settings → Quick filters, deadlines and lifecycle.
lifecycle-settings-expire-to = Expired messages
lifecycle-settings-expire-to-hint = Where a message goes when “Expires” comes — its own date or a confirmation code.
lifecycle-settings-to-archive = To Archive
lifecycle-settings-to-trash = To Deleted Items
lifecycle-settings-remind = Reminders
lifecycle-settings-remind-hint = “Remind me…” moves the message to Archive and brings it back unread on top of the Inbox on time.
lifecycle-settings-remind-off = Off: at the time only a toast reminds you, the message stays where it is.
