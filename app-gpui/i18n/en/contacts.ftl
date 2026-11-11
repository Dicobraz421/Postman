# Contacts (S4-02, look — S5-11): personal contacts (JMAP ContactCard) and the staff directory (Principals).
contacts-account = Contacts
contacts-mine = My contacts
contacts-directory = Company directory
contacts-nav-note = The directory comes from the server and updates itself. Your edits live in "My contacts": "Edit" on an employee creates a personal copy.
contacts-search-placeholder = Search contacts
contacts-new = New contact
contacts-directory-search-only = The directory is available by search: type a name or address in Search contacts.
contacts-loading = Loading contacts…
contacts-empty = Nobody here yet.
contacts-nothing = Nobody found
contacts-favorites = Favorites
contacts-frequent = Frequent
contacts-frequent-summary = last { $days } days
contacts-sort-frequency = By frequency
contacts-sort-address = By address
contacts-sort-recent = Recent
contacts-search-title = Search
contacts-group-correspondents = Correspondence
contacts-kind-correspondent = Correspondent
contacts-count = { $n } total
contacts-found = { $n } found · name, address, phone, company
contacts-sort-name = By name
contacts-sort-match = By match
contacts-pick = Select a contact
contacts-no-name = (no name)
contacts-kind = Type
contacts-kind-contact = Personal contact
contacts-kind-person = Employee
contacts-kind-room = Meeting room
contacts-kind-resource = Resource
contacts-emails = Email
contacts-organization = Company
contacts-title = Job title
contacts-write = Write
contacts-meet = Meeting
contacts-copy = Copy address
contacts-copied = Address copied: { $email }
contacts-edit = Edit
contacts-delete = Delete
contacts-add-mine = Add to my contacts
contacts-badge-mine = My contacts
contacts-badge-directory = From the directory · read only
contacts-book-room = Book
contacts-now = now { $time }
contacts-no-titles = meeting titles are not visible
contacts-busy-hidden = Free/busy hidden: the colleague has not shared the calendar with you.
contacts-delete-confirm = Delete contact “{ $name }”?
contacts-yes = Yes
contacts-no = No
contacts-readonly = Organization directory — read only.
contacts-form-new = New contact
contacts-form-edit = Edit contact
contacts-form-where = Saved to "My contacts" on the server
contacts-form-where-demo = Demo: saved in this window only
contacts-form-hint = Lists show the name as “Last First”. Several addresses or phones — separated by “;”. Enter saves, Esc cancels.
contacts-emails-placeholder = name@example.com; …
contacts-phones-placeholder = +1 …; …
contacts-save = Save
contacts-cancel = Cancel
contacts-saving = Saving…
contacts-deleting = Deleting…
contacts-need-name = Enter a name or an address.
contacts-bad-email = Invalid address: { $email }
contacts-saved = Contact saved: { $name }
contacts-deleted = Contact deleted: { $name }
contacts-failed = Contacts: { $error }
contacts-demo-local = Demo: contacts change in this window only, nothing goes to a server.
# Mockup check T2-G (T2-40…T2-46).
contacts-recent-mail = Recent mail
contacts-all-mail = All mail
contacts-no-mail = No mail yet.
contacts-mail-loading = Looking for mail…
contacts-you = You
contacts-lists = Mailing lists
contacts-lists-all = All lists
contacts-kind-list = Mailing list
contacts-list-members = The server does not list the members: mail to this address reaches everyone on the list.
contacts-presence-me = it's you
contacts-presence-free = { $f ->
   *[other] free
}
contacts-presence-free-long = { $f ->
   *[other] Free
}
contacts-presence-busy = { $f ->
   *[other] busy
} until { $time }
contacts-presence-busy-long = { $f ->
   *[other] Busy
} until { $time }
contacts-presence-away = away until { $time }
contacts-presence-away-long = Out of office until { $time }
contacts-presence-hidden = availability hidden
contacts-presence-date = { $month } { $day }
contacts-window = · next free slot { $from }–{ $to }
contacts-meet-at = Meet at { $time }
contacts-manager = Manager
contacts-office = Office
contacts-department = Department
contacts-work-phone = Work phone
contacts-mobile = Mobile
contacts-last-name = Last name
contacts-first-name = First name
contacts-book = Address book
contacts-layout-fixed = layout fixed: { $text }
contacts-layout-reset = Clear search
contacts-directory-updated = Directory updated { $time }
# Personal groups (T2-47).
contacts-groups = Groups
contacts-groups-all = All groups
contacts-new-group = New group
contacts-kind-group = Group
contacts-badge-group = My group
contacts-group-count = { $n ->
    [one] { $n } member
   *[other] { $n } members
}
contacts-group-summary = Group · { $n ->
    [one] { $n } member
   *[other] { $n } members
}
contacts-group-members = Members
contacts-group-empty = No one in the group yet: "Edit" to add members, or "To group" on a contact.
contacts-group-remove = Remove from group
contacts-group-foreign = Member from another app
contacts-group-hint = A message to the group goes to every member: recipients see the members' addresses, not the group. A group address on the server is a "Distribution list".
contacts-write-group = Write to group
contacts-group-form-new = New group
contacts-group-form-edit = Edit group
contacts-group-form-where = Saved to "My contacts" on the server
contacts-group-form-hint = Members separated by ";": an address, "Name <address>" or a name from the directory and contacts. Enter saves, Esc cancels.
contacts-group-name = Name
contacts-group-name-placeholder = For example, Department heads
contacts-group-members-placeholder = addresses or names separated by ";"
contacts-group-need-name = Enter a group name
contacts-group-saved = Group saved: { $name }
contacts-group-delete-confirm = Delete group "{ $name }"? The contacts stay.
contacts-to-group = To group
contacts-to-group-new = New group…
contacts-added-to-group = { $name } added to "{ $group }"
contacts-already-in-group = { $name } is already in "{ $group }"
contacts-removed-from-group = { $name } removed from "{ $group }"
