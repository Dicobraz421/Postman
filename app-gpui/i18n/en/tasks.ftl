# Tasks (S6-05, Ctrl+4): lists, projects, categories, grouped list, task card.
tasks-inbox = Inbox
tasks-today = Today
tasks-upcoming = Upcoming
tasks-from-mail = From mail
tasks-someday = Someday
tasks-done-list = Completed
tasks-projects = Projects
tasks-categories = Categories
tasks-nav-note = An email with a due date becomes a task by itself. Complete the task and the email's due date is cleared.
tasks-search = Search tasks
tasks-quick-placeholder = Add a task
tasks-new = New task
tasks-complete = Complete
tasks-reopen = Reopen
tasks-edit = Edit
tasks-delete = Delete
tasks-pick = Pick a task on the left.
tasks-count = { $n ->
    [one] { $n } task
   *[other] { $n } tasks
}
tasks-summary = { tasks-count }
tasks-summary-today = { $day } · { tasks-count }
tasks-summary-today-over = { $day } · { tasks-count } · { $over } overdue
tasks-summary-mail = emails with a due date · { tasks-count }
tasks-summary-project = { $done } of { $total } done
tasks-group-overdue = Overdue
tasks-group-today = Today
tasks-group-timed = by time
tasks-group-no-time = Any time
tasks-group-tomorrow = Tomorrow
tasks-group-soon = Soon
tasks-group-later = Later
tasks-group-no-due = No due date
tasks-group-yesterday = Yesterday
tasks-group-earlier = Earlier
tasks-empty-inbox = Inbox is empty. Tasks without a due date or project land here — sort them out when you have a minute.
tasks-empty-today = All done for today. Have a nice evening.
tasks-empty-upcoming = Nothing ahead.
tasks-empty-mail = No emails with a due date. Set a due date on an email and it shows up here.
tasks-empty-someday = Ideas and tasks without a due date go here.
tasks-empty-done = Nothing completed yet.
tasks-empty = No tasks here.
tasks-kind-task = Task
tasks-kind-mail = From mail
tasks-letter = Email
tasks-open = Open
tasks-letter-gone = The email is no longer in the mailbox.
tasks-field-due = Due
tasks-field-remind = Reminder
tasks-field-repeat = Repeat
tasks-field-project = Project
tasks-field-category = Category
tasks-due-at = { $day }, by { $time }
tasks-no-due = No due date
tasks-no-remind = None
tasks-remind-set = { $n ->
    [one] { $n } reminder
   *[other] { $n } reminders
}
tasks-repeats = Repeats
tasks-no-repeat = Does not repeat
tasks-no-project = No project
tasks-no-category = No category
tasks-progress = { $done } of { $total }
tasks-subtasks = Subtasks
tasks-no-subtasks = No subtasks
tasks-notes = Notes
tasks-no-notes = No notes
tasks-done = Done · “{ $title }”
tasks-done-mail = Done · the email's due date is cleared
tasks-reopened = The task is open again
tasks-deleted = Task deleted
tasks-deleted-mail = Deleted · the email's due date is cleared
tasks-status-mail = { $n ->
    [one] { $n } task from emails
   *[other] { $n } tasks from emails
}
tasks-failed = The task was not saved on the server: { $error }
tasks-project-new = New project — Enter
tasks-sub-add = Add a subtask
tasks-notes-placeholder = Add a note…
tasks-important = important
tasks-remind-at-due = At due time
tasks-remind-minutes = { $n } min before
tasks-remind-hours = { $n } h before
tasks-remind-days = { $n } d before
tasks-remind-at = At { $when }
tasks-block = Time in calendar
tasks-block-remove = Remove
tasks-block-none = No free slot left today
tasks-block-placed = In calendar: { $when }
tasks-done-next = Done · next — { $when }
tasks-repeat-daily = Every day
tasks-repeat-days = Every { $n } days
tasks-repeat-weekdays = Weekdays
tasks-repeat-weekly = Every week
tasks-repeat-weeks = Every { $n } weeks
tasks-repeat-monthly = Every month
tasks-repeat-months = Every { $n } months
tasks-repeat-monthly-day = Every month on day { $day }
tasks-repeat-yearly = Every year
tasks-repeat-after = { $n } d after completion
tasks-repeat-other = Repeats (custom rule)
tasks-snooze = Snooze 10 min
tasks-remind-title = Task reminder
tasks-remind-toast = Reminder: “{ $title }”


# Delegation (S6-10): the Delegate form, the delegation card, step emails.
tasks-delegate = Delegate
tasks-remind = Remind
tasks-revoke = Take back
assign-you = You
assign-chip-mine = Delegated by you
assign-chip-theirs = Delegated to you
assign-chip-watch = You are watching
assign-watch-owner = Assigned by { $name }
assign-watch-owner-in = Assigned by { $name } · { $project }
assign-role-assignee = Assignee
assign-role-owner = Delegated by
assign-st-waiting = Waiting for acceptance
assign-st-proposed = New due date proposed
assign-st-accepted = Accepted
assign-st-in-progress = In progress
assign-st-done = Done — for review
assign-st-declined = Declined
assign-st-closed = Accepted by you
assign-st-revoked = Taken back
assign-me-st-waiting = Waiting for your answer
assign-me-st-proposed = You proposed a due date
assign-me-st-accepted = You accepted
assign-me-st-in-progress = In progress
assign-me-st-done = Under review
assign-me-st-declined = You declined
assign-me-st-closed = Result accepted
assign-me-st-revoked = Delegation withdrawn
assign-rib-1 = Delegated
assign-rib-2 = Accepted
assign-rib-3 = In progress
assign-rib-4 = Done
assign-rib-5 = Accepted by you
assign-rib-declined = Declined
assign-rib-checked = Reviewed
assign-field-due = Due
assign-field-proposed = Proposed due date
assign-field-watchers = Observers
assign-none = None
assign-history = History
assign-step = { $who } — { $what }
assign-what-assigned = delegated
assign-what-accepted = accepted
assign-what-declined = declined
assign-what-proposed = proposed a due date: { $due }
assign-what-started = in progress
assign-what-finished = done, for review
assign-what-recalled = recalled from review
assign-what-reopened = answer changed
assign-what-reminded = reminder
assign-what-due-changed = new due date: { $due }
assign-what-agreed = due date agreed: { $due }
assign-what-kept = due date kept
assign-what-result-accepted = result accepted, task closed
assign-what-returned = returned for rework
assign-what-revoked = delegation taken back
assign-what-reassigned = delegated to someone else
assign-note-result = Result
assign-note-proposed = Proposes a due date
assign-note-declined = Reason
assign-note-returned = What to rework
assign-note-assigned = Delegation note
assign-accept-result = Accept result
assign-return = Return with a comment
assign-do-return = Return
assign-change-due = Change due date
assign-revoke = Take the task back
assign-agree = Agree: { $due }
assign-keep = Keep the due date
assign-reassign = Delegate to someone else
assign-remind = Remind
assign-reminded = Reminded { $time }
assign-accept = Accept
assign-propose = Propose a due date
assign-decline = Decline
assign-start = In progress
assign-finish = Done — send for review
assign-recall = Recall from review
assign-reopen = Change answer
assign-send = Send
assign-cancel = Cancel
assign-delegate = Delegate
assign-new-due = New due date
assign-due-note = { $who } will see the new due date in the task
assign-pick-due = Pick a due date
assign-comment-return = What to rework — { $who } will see it
assign-comment-decline = Reason — { $who } will see it
assign-comment-propose = Comment — { $who } will see it
assign-sent-review = Sent for review
assign-proposed-due = You proposed: { $due }
assign-closed = Result accepted — task closed
assign-closed-theirs = The author accepted the result — task closed
assign-revoked = The author withdrew the delegation
assign-revoked-mine = Taken back — the task is yours again
assign-due-today = Today
assign-due-tomorrow = Tomorrow
assign-due-text = { $day }, by { $time }
assign-form-title = Delegate
assign-form-person = Assignee
assign-form-person-placeholder = Name, department or address
assign-form-person-hint = Directory and your contacts · keyboard layout does not matter
assign-form-watchers = Observers
assign-form-watchers-opt = · optional
assign-form-watcher-placeholder = Add — they follow the progress, not do the work
assign-form-due = Due
assign-form-comment = Comment
assign-form-comment-placeholder = What matters — the assignee will see it
assign-form-note = { $who } will see the task in “Assigned to me”
assign-form-note-empty = One assignee
assign-form-pick-first = Pick an assignee first
assign-letter-first = An email with a due date cannot be delegated: open it as a task first (a note or a project)
assign-invalid = This action is not available now
assign-too-soon = Already reminded — the next reminder is possible after { $time }
assign-not-yet = The delegation is not in your tasks yet — it will appear in a few seconds
assign-failed = The delegation email was not sent: { $error }
assign-toast-saved = Delegation saved
assign-toast-demo = { $text } · demo: no email sent
assign-toast-assigned = Delegated: “{ $title }” — waiting for acceptance
assign-toast-accepted = You accepted “{ $title }”
assign-toast-declined = You declined “{ $title }”
assign-toast-proposed = New due date proposed: “{ $title }”
assign-toast-started = “{ $title }” is in progress
assign-toast-finished = “{ $title }” is sent for review
assign-toast-recalled = “{ $title }” is recalled from review
assign-toast-reopened = Answer for “{ $title }” changed
assign-toast-reminded = Reminded about “{ $title }”
assign-toast-due-changed = New due date for “{ $title }” — the assignee will see it
assign-toast-agreed = Due date agreed: “{ $title }”
assign-toast-kept = Due date of “{ $title }” kept
assign-toast-result-accepted = Result accepted: “{ $title }” closed
assign-toast-returned = “{ $title }” returned for rework
assign-toast-revoked = “{ $title }” — the task is yours again
assign-toast-reassigned = “{ $title }” delegated to someone else
assign-mail-card-request = Delegation
assign-mail-card-reply = Delegation reply
assign-mail-card-counter = New due date proposed
assign-mail-card-cancel = Delegation withdrawn
assign-mail-card-declinecounter = Due date kept
assign-mail-from = From: { $who }
assign-open-tasks = Open in Tasks
assign-mail-due = Due: { $due }
assign-mail-comment = Comment: { $text }
assign-mail-footer = If you do not use Postman, reply to this email — { $who } will see your answer.
assign-mail-subject-assigned = Task for you: { $title }
assign-mail-subject-accepted = Accepted: { $title }
assign-mail-subject-declined = Declined: { $title }
assign-mail-subject-proposed = New due date proposed: { $title }
assign-mail-subject-started = In progress: { $title }
assign-mail-subject-finished = Done, for review: { $title }
assign-mail-subject-recalled = Recalled from review: { $title }
assign-mail-subject-reopened = Answer changed: { $title }
assign-mail-subject-reminded = Reminder: { $title }
assign-mail-subject-due-changed = New due date: { $title }
assign-mail-subject-agreed = Due date agreed: { $title }
assign-mail-subject-kept = Due date kept: { $title }
assign-mail-subject-result-accepted = Result accepted: { $title }
assign-mail-subject-returned = For rework: { $title }
assign-mail-subject-revoked = Delegation withdrawn: { $title }
assign-mail-subject-reassigned = Task for you: { $title }
assign-mail-body-assigned = { $who } delegates the task “{ $title }” to you.
assign-mail-body-accepted = { $who } accepts “{ $title }”.
assign-mail-body-declined = { $who } declines “{ $title }”.
assign-mail-body-proposed = { $who } proposes a new due date for “{ $title }”.
assign-mail-body-started = { $who } started working on “{ $title }”.
assign-mail-body-finished = { $who }: “{ $title }” is done and sent to you for review.
assign-mail-body-recalled = { $who } recalls “{ $title }” from review — back in progress.
assign-mail-body-reopened = { $who } changes the answer for “{ $title }”.
assign-mail-body-reminded = { $who } reminds you about “{ $title }”.
assign-mail-body-due-changed = { $who } changes the due date of “{ $title }”.
assign-mail-body-agreed = { $who } agrees to the new due date of “{ $title }”.
assign-mail-body-kept = { $who } keeps the due date of “{ $title }”.
assign-mail-body-result-accepted = { $who } accepts the result: “{ $title }” is closed. Thank you!
assign-mail-body-returned = { $who } returns “{ $title }” for rework.
assign-mail-body-revoked = { $who } takes the task “{ $title }” back — no need to do it.
assign-mail-body-reassigned = { $who } delegates the task “{ $title }” to you.
# Screen `delegate`: the form comment (demo).
assign-screen-comment = Collect questions for the customer: delivery dates, requirements, licences and SLA.

# Delegations section (S6-11).
tasks-delegations = Delegations
deleg-count = { $n ->
    [one] { $n } delegation
   *[other] { $n } delegations
}
deleg-summary-by-me = { $review ->
    [0] { deleg-count }
   *[other] { deleg-count } · { $review } to review
}
deleg-summary-to-me = { $new ->
    [0] { deleg-count }
   *[other] { deleg-count } · { $new } awaiting your answer
}
deleg-tab-by-me = Delegated by me
deleg-tab-to-me = Assigned to me
deleg-own-only = Mine only
deleg-group-review = To review
deleg-group-review-sub = waiting for your decision
deleg-group-work = In progress
deleg-group-waiting = Not accepted yet
deleg-group-new = New
deleg-group-new-sub = waiting for your answer
deleg-group-checking = Under review
deleg-group-checking-sub = by the author
deleg-remind-all = Remind everyone
deleg-watching = watching
deleg-last = { $when } — { $what }
deleg-last-watched = by { $who } · { $last }
deleg-proposes = Proposes: { $due }
deleg-wait-hint = { $n ->
    [one] An open delegation ({ $n }) shows up in mail — in the “Waiting” quick filter
   *[other] Open delegations ({ $n }) show up in mail — in the “Waiting” quick filter
}
deleg-open-wait = Open in mail
deleg-status-waiting = In “Waiting”: { deleg-count }
deleg-empty-by-me = No delegations. Open a task and press “Delegate”.
deleg-empty-to-me = Nothing is assigned to you.
assign-toast-taken = The task is yours again · in “{ $list }”
assign-toast-deleted = Delegation deleted · { $who } will see it
assign-delete-not-mine = Not your delegation: its author deletes it. You can decline it.
assign-withhold-late = The message has already gone — the assignee got the cancellation
assign-toast-reminded-all = { $n ->
    [one] Reminded { $n } assignee
   *[other] Reminded { $n } assignees
}{ $soon ->
    [0] {""}
   *[other] {" "}· { $soon } already today
}
assign-remind-all-soon = Everyone was reminded today — next reminder in a day
