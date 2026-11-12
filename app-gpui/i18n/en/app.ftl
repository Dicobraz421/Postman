# Mail screen.
folders-results = 🔍 Results ({ $n })
reading-empty = Select a message
reading-from = From:
reading-to = To:
reading-date = Date:
reading-remote-blocked = Remote images are not loaded ({ $n }). This protects you from tracking.
reading-image = [image]
reading-image-alt = [image: { $alt }]
reading-error = Message not shown: { $error }
worker-missing = Messages, images and attachments are not shown: the message parsing component { $file } is missing. Cause: the file was removed from the program folder or not extracted from the archive. Fix: download { $file } and put it into { $dir }; no need to restart Postman.
worker-missing-letter = Message not shown: a program component is missing. To see what happened and how to fix it, click “Component missing” in the status bar.
worker-download = Download
status-worker-missing = Component missing
status-worker-tip = Messages are not shown — click to see how to fix it
sandbox-full = Sandbox: full
sandbox-partial = Sandbox: partial
sandbox-none = Sandbox: none
sandbox-weak = The message sandbox is weakened ({ $level }): messages open with less protection.
status-items = Items: { $n }
status-accounts = { $n ->
    [one] { $n } account
   *[other] { $n } accounts
}
status-shared = { $n ->
    [one] { $n } shared mailbox
   *[other] { $n } shared mailboxes
}
status-unread = unread: { $n }
status-cached = (cached)
status-notifications-hint = Notifications
second-instance = Postman is already running — this window is shown.
# Tray icon (T3-13).
tray-open = Open Postman
tray-quit = Quit
tray-tip-unread = Postman — unread: { $n }
tray-hint-title = Postman is running in the background
tray-hint-body = The window is closed, mail keeps syncing. The icon is in the notification area; use its menu to quit.

# Startup failure (Windows event log and stderr).
fatal-data-dir = data directory
fatal-bench-mailbox = bench mailbox
fatal-screens-dir = screenshots directory
fatal-demo-mailbox = demo mailbox
fatal-store = store
fatal-window = window
needs-server = This needs a server account: not available in demo mode.
# «Нет у сервера — нет на экране» (S7-03): клавиша команды, которой у сервера нет.
feature-unavailable = Not available on this account's server
# System folders (server's default name in the interface language)
folder-role-inbox = Inbox
folder-role-sent = Sent
folder-role-drafts = Drafts
folder-role-trash = Deleted
folder-role-junk = Junk
folder-role-archive = Archive
tab-dirty-tip = Unsaved changes
thread-count-tip = Messages in the conversation
