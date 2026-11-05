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
cal-group-categories = Categories
cal-group-share = Free/busy
cal-share-open = Share my free/busy
cal-share-label = Share my free/busy with a colleague
cal-share-placeholder = colleague's address
cal-share-button = Share
cal-share-cancel = Cancel
cal-share-note = A colleague sees your free/busy only if you shared it (without meeting details). Until a colleague shares theirs, the assistant shows "no data" for them, not "free".
cal-share-bad = Not an address: { $email }
cal-share-sending = Sharing free/busy: { $email }…
cal-share-done = Your free/busy is shared: { $email }

# Week.
cal-all-day-1 = All
cal-all-day-2 = day
cal-all-day = all day
cal-waits-answer = awaiting reply

# Event details.
cal-details-empty = Pick a meeting to see its details here.
cal-details-recurring = Recurring event
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

# Meeting tab.
meeting-title-new = New meeting
meeting-tab-titled = New meeting · { $title }
meeting-send = Send
meeting-send-tip = Send invitations
meeting-subject = Subject
meeting-subject-placeholder = Meeting subject
meeting-required = Required
meeting-optional = Optional
meeting-rooms = Room
meeting-location = Location / link
meeting-location-placeholder = Room or link
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
invite-done-tentative = You answered "Tentative"
invite-done-declined = You declined
invite-route = The reply goes through the server calendar · from invite.ics, parsed in the sandbox
invite-route-done = The reply is stored in the server calendar by the event UID · from invite.ics, parsed in the sandbox
invite-cancelled = Meeting cancelled by the organizer · { $organizer }
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
meeting-link-title = Free/busy link for { $email }
meeting-link-placeholder = https://… — FBURL or an .ics calendar
meeting-link-bad = Needs an https:// link with a server name
meeting-link-save = Save
meeting-link-remove = Remove
meeting-link-cancel = Cancel
