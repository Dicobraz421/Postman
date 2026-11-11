# Calendar (S3-05, look — S5-11): week, event details, sharing free/busy.
cal-today = Today
cal-today-tip = Go to today
cal-new-meeting = New meeting
cal-new-meeting-tip = New meeting

# Views and search (S5-21).
cal-view-day = Day
cal-view-day-full = Day
cal-view-work-week = Work week
cal-view-work-week-full = Work week
cal-view-week = Week
cal-view-week-full = Week
cal-view-month = Month
cal-view-month-full = Month
cal-month-more = +{ $n } more
cal-short-day = { $wd }, { $month } { $day }
cal-search-placeholder = Search the calendar
cal-search-found = Found: { $n }
cal-search-nothing = Nothing found. Searching subject, place, description and attendees.
cal-search-server = Searching the server…
cal-search-failed = The server did not answer ({ $error }) — showing matches in loaded days
cal-search-scope-server = On the server — a year back and ahead; Enter opens the event
cal-search-scope-loaded = All calendar events; Enter opens the event
cal-legend-accepted = Accepted: { $n }
cal-legend-tentative = Tentative: { $n }
cal-legend-declined = Declined: { $n }
cal-legend-none = No answer: { $n }
cal-loading = Loading…
cal-day = { $d ->
    [0] Mon
    [1] Tue
    [2] Wed
    [3] Thu
    [4] Fri
    [5] Sat
   *[6] Sun
}
cal-weekday = { $d ->
    [0] Monday
    [1] Tuesday
    [2] Wednesday
    [3] Thursday
    [4] Friday
    [5] Saturday
   *[6] Sunday
}
cal-month = { $m ->
    [1] January
    [2] February
    [3] March
    [4] April
    [5] May
    [6] June
    [7] July
    [8] August
    [9] September
    [10] October
    [11] November
   *[12] December
}
cal-month-of = { $m ->
    [1] January
    [2] February
    [3] March
    [4] April
    [5] May
    [6] June
    [7] July
    [8] August
    [9] September
    [10] October
    [11] November
   *[12] December
}
cal-month-title = { $month } { $year }
cal-long-day = { $wd }, { $month } { $day }
cal-minutes = { $n } min
cal-hours = { $n } h
cal-busy = busy
cal-tentative = tentative
cal-unavailable = out of office
cal-failed = Calendar: { $error }

# Left: my calendars, categories, free/busy.
cal-my-calendar = My calendar
cal-group-mine = My calendars
cal-group-shared = Shared
cal-status = { $view } · events: { $n } · awaiting reply: { $waiting } · declined hidden: { $declined } · { $zone }
cal-block-owner = { $owner } calendar: { $text }
cal-block-pending = { $text } — invitation not answered
cal-block-tentative = { $text } — tentative
cal-group-categories = Categories
cal-group-share = Calendar access
cal-share-open = Share calendar with a colleague
cal-share-label = Share my calendar with a colleague
cal-share-placeholder = colleague's address
cal-share-button = Share
cal-share-cancel = Cancel
cal-share-note = A colleague sees your calendar only if you shared it. Until a colleague shares theirs, the assistant shows "no data" for them, not "free".
cal-share-bad = Not an address: { $email }
cal-share-sending = Sharing calendar: { $email }…
cal-share-done = Calendar shared: { $email }

# Week.
cal-all-day-1 = All
cal-all-day-2 = day
cal-all-day = all day
cal-waits-answer = awaiting reply

# Event details.
cal-details-empty = Pick a meeting to see its details here.
cal-details-recurring = Recurring event
cal-details-repeat-until = { $rule }, until { $date }
cal-details-repeat-count = { $rule }, { $n ->
    [one] { $n } time
   *[other] { $n } times
}
cal-join = Join
cal-join-tip = Open the video meeting link in the browser
cal-chip-organizer = You organize
cal-chip-waits = Awaiting reply
cal-chip-tentative = Tentative
cal-chip-declined = Declined
cal-chip-accepted = Accepted
cal-invited-by = { $name } invites you
cal-accept = Accept
cal-tentative-answer = Tentative
cal-decline = Decline
cal-answered-accepted = You accepted — the organizer is notified
cal-answered-tentative = You answered "Tentative" — the organizer is notified
cal-answered-declined = You declined — the event is hidden from the grid
cal-change-answer = Change reply
cal-conflict = Overlaps "{ $title }", { $time }
cal-conflict-more = and { $n } more
cal-participants-n = Attendees · { $n }
cal-you = { $name } (you)
cal-organizer = organizer
cal-optional = optional
cal-external = External
cal-write-participants = Email attendees
cal-details-reminder = Reminder { $time } before
cal-details-reminder-start = Reminder at the start
cal-details-organizer-time = Organizer’s time: { $time } · { $zone }
cal-open-event = Open
cal-details-imip = Came by email — the reply will be mailed to the organizer
cal-open-letter-none = The letter with this invitation is not in the mailbox

# Meeting tab.
meeting-title-new = New meeting
meeting-tab-titled = New meeting · { $title }
meeting-send = Send
meeting-send-tip = Send invitations
meeting-all-day = All day
meeting-private = Private
meeting-private-tip = Private: others see only “Busy”
meeting-repeat = Repeat
meeting-repeat-none = Does not repeat
meeting-repeat-end-never = No end
meeting-repeat-end-until = Until a date…
meeting-repeat-end-count = N times
meeting-repeat-until = until
meeting-repeat-times = times
meeting-subject = Subject
meeting-subject-placeholder = Meeting subject
meeting-required = Required
meeting-optional = Optional
meeting-rooms = Room
meeting-location = Location / link
meeting-location-placeholder = Room or link
meeting-new-link = New link
meeting-video = Video meetings
meeting-video-placeholder = Video meeting service: https://meet.company.com — Enter
meeting-zone = Meeting time zone
meeting-draft-saved = Draft saved { $time }
meeting-when = Start – end
meeting-date-placeholder = dd.mm.yyyy
meeting-start-placeholder = hh:mm
meeting-duration = Duration
meeting-description = Description
meeting-desc-add = Add description
meeting-desc-hide = Collapse description
meeting-desc-hint = agenda, materials, links
meeting-description-placeholder = Meeting agenda
meeting-attendees-placeholder = Address or name — Enter or ";"
meeting-external-short = external
meeting-presence-tip = { $name } — { $state } at the picked time
meeting-presence-free = free
meeting-presence-unknown = free/busy unknown
meeting-presence-external = external, free/busy unknown
meeting-me = You
meeting-role-organizer = Organizer
meeting-role-required = Required
meeting-role-optional = Optional
meeting-role-room = Room
meeting-not-counted = { $role } · not counted
meeting-only-free = Common free time only
meeting-with-optional = Count optional attendees
meeting-legend-free = Everyone free
meeting-legend-busy = Busy
meeting-legend-unknown = Unknown
meeting-legend-external = External
meeting-not-counted-n = Not counted: { $n } — free/busy unknown
meeting-slots-hint = { $prev } { $next } — through free slots, { $pick } — pick
meeting-participants-n = Attendees · { $n }
meeting-everyone = All attendees
meeting-known = { $known } of { $n } with free/busy data
meeting-add-person = Add attendee
meeting-row-hidden = Calendar not shared with you — "no data", not "free"
meeting-row-external = External address — the invitation goes by email
meeting-external-note = External attendees get the invitation by email
meeting-busy-loading = Requesting free/busy…
meeting-busy-failed = Free/busy not received: { $error }
meeting-sending = Sending invitations…
meeting-bad-time = Date or time not recognized: { $text }
meeting-bad-repeat-end = Could not read the end of the series: { $text }
meeting-bad-addresses = Not addresses: { $list }
meeting-no-server = A server is needed: invitations are not sent in demo mode.
meeting-failed = Not sent: { $error }
meeting-sent = Invitations sent: { $title }
meeting-created = Meeting created: { $title }
meeting-untitled = (no subject)

# Invitation in a letter (card, S5-11).
invite-day = { $wd }, { $month } { $day }
invite-month-short = { $m ->
    [1] Jan
    [2] Feb
    [3] Mar
    [4] Apr
    [5] May
    [6] Jun
    [7] Jul
    [8] Aug
    [9] Sep
    [10] Oct
    [11] Nov
   *[12] Dec
}
invite-organizer = Organizer: { $name }
invite-accepted-of = { $n } of { $total } accepted
invite-updated = The organizer changed: { $what }
invite-changed-time = time
invite-changed-place = location
invite-changed-title = subject
invite-done-accepted = You accepted
invite-more = Response options
invite-more-all = More responses
invite-with-comment = { $s ->
    [accepted] Accept with a comment…
    [tentative] Tentative with a comment…
   *[declined] Decline with a comment…
}
invite-no-reply = { $s ->
    [accepted] Accept without replying
    [tentative] Tentative without replying
   *[declined] Decline without replying
}
invite-comment-title = { $s ->
    [accepted] Accept — comment to the organizer
    [tentative] Tentative — comment to the organizer
   *[declined] Decline — comment to the organizer
}
invite-comment-placeholder = For example: I will be 10 minutes late
invite-comment-send = Send
invite-comment-cancel = Cancel
invite-sample-comment = I have a court hearing then — can we move it to Thursday?
invite-done-tentative = You answered "Tentative"
invite-done-declined = You declined
invite-route = The reply goes through the server calendar · from invite.ics, parsed in the sandbox
invite-route-done = The reply is stored in the server calendar by the event UID · from invite.ics, parsed in the sandbox
invite-cancelled = Meeting cancelled by the organizer · { $organizer }
invite-queued = { $answer } · { $key } to undo
invite-queued-accepted = Accepted
invite-queued-tentative = Tentative
invite-queued-declined = Declined
invite-queued-silent = organizer not notified
invite-queued-comment = with a comment
invite-answer-undone = Reply undone — not sent
invite-external = External invitation
invite-external-from = External invitation · { $from }
invite-external-replied = the reply was mailed to the organizer
invite-block-own = { $title } · { $time }
invite-block-invite = { $title } · { $time } · this invitation
invite-block-overlap = { $title } · { $time } · overlaps the invitation { $overlap }
invite-remove = Remove from calendar
invite-removing = Removing…
invite-remove-hint = Remove the cancelled meeting from your calendar. Nothing is sent to the organizer
invite-removed = Meeting removed from the calendar
invite-remove-none = This meeting is not in your calendar
list-meeting-cancelled = Cancelled · { $when }
invite-outdated-newer = Outdated invitation: the calendar has a newer version of the meeting
invite-outdated-past = The meeting is over
invite-day-loading = Loading the calendar…
invite-conflict = Overlaps: { $title } { $time }
invite-conflict-many = Overlaps: { $title } { $time } and { $n } more
invite-no-conflict = No overlaps
invite-sample-old-place = Room 2
invite-your-day-title = Your day
invite-chip-waiting = Awaiting reply
invite-chip-accepted = Accepted
invite-chip-tentative = Tentative
invite-chip-declined = Declined
invite-route-short = The reply goes through the server calendar
invite-route-short-done = Stored in the server calendar
invite-side-title = Invitation
invite-side-strip = Meeting invitation · { $when }
invite-side-strip-hint = Details and reply are in the panel on the right
invite-now = Now { $time }
cal-group-colleagues = Colleagues’ calendars
cal-colleague-calendar = Calendar: { $name }
cal-private-other = Private event — the owner hid the details
cal-remind-title = Meeting reminder
cal-remind-soon = In { $n } min: { $title } — { $when }
cal-remind-now = Starting: { $title } — { $when }
cal-open-colleague = Open a colleague’s calendar
cal-open-placeholder = Employee name or address
cal-open-bad = Pick an employee from the suggestions or type an address
cal-open-already = Calendar is already open: { $name }
cal-open-close = Close calendar
cal-fb-busy = Busy
cal-fb-tentative = Tentative
cal-fb-away = Out of office
cal-fb-hidden = Availability hidden
cal-fb-hidden-tip = The server returned no availability: the calendar is not shared with you or the employee was not found. This does not mean free.
cal-fb-only = Only availability is visible: the colleague did not share details
cal-fb-some = Subject and location are visible: the colleague did not share the rest
cal-chip-colleague = Colleague’s calendar
cal-color-change = Calendar color

# Правка и отмена своей встречи (S7-07б)
cal-edit-meeting = Edit
cal-cancel-meeting = Cancel meeting
cal-edit-series-ask = Recurring meeting — edit:
cal-cancel-series-ask = Cancel the recurring meeting? Attendees will get a cancellation.
cal-cancel-ask = Cancel the meeting? Attendees will get a cancellation.
cal-series-one = This one
cal-series-all = The whole series
cal-cancel-keep = Don't cancel
cal-cancel-back = Back
cal-cancelling = Cancelling the meeting…
cal-meeting-cancelled = Meeting cancelled, attendees notified
cal-cancel-failed = Meeting not cancelled: { $error }
meeting-title-edit = Edit meeting
meeting-tab-edit-titled = Edit meeting · { $title }
meeting-updated = Changes sent: { $title }
# External free/busy by link (S8-42)
meeting-by-link = { $role } · by link
meeting-link-change = Free/busy link for { $email }
meeting-link-set = Set link
meeting-link-loading = Loading free/busy from the link…
meeting-link-failed = Free/busy link is unavailable
meeting-late = starts at { $time } — 5 min late
meeting-link-title = Free/busy link for { $email }
meeting-link-placeholder = https://… — FBURL or an .ics calendar
meeting-link-bad = Needs an https:// link with a server name
meeting-link-save = Save
meeting-link-remove = Remove
meeting-link-cancel = Cancel
# Calendar subscriptions (S8-44)
cal-group-subscriptions = Subscriptions
cal-sub-open = Add by link
cal-sub-placeholder = https://… or webcal://…
cal-sub-add = Add
cal-sub-bad = Needs an https:// or webcal:// link with a server name
cal-sub-failed = Not updated, showing earlier events
cal-sub-refresh = Refresh
cal-sub-remove = Remove
cal-sub-new = Calendar by link
cal-chip-subscription = Subscription
# Calendar access (S8-43)
cal-share-items = Include meeting details
cal-access-busy = Free/busy only
cal-access-items = Free/busy and details
cal-access-close = Remove access
# Calendar publishing (S8-45)
cal-group-publish = Publish by link
cal-pub-add-busy = Create link: free/busy only
cal-pub-add-items = Create link: with details
cal-pub-revoke = Revoke
cal-pub-copy = Copy link
cal-pub-once = The link is shown once. Anyone you give it to can open it.
