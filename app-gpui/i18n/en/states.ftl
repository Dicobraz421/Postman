# States (S5-08): offline, the notification stack and its history.
states-offline-retry = Offline · retrying in { $n } s
states-offline-trying = Offline · connecting…
states-offline-queued = { $n ->
    [one] { $n } change waiting to be sent
   *[other] { $n } changes waiting to be sent
}
states-banner = No connection to { $host }. Showing cached mail; flags, moves and deletions go to the server when the connection is back.
states-banner-queued = { $n ->
    [one] No connection to { $host }. Showing cached mail; { $n } change goes to the server when the connection is back.
   *[other] No connection to { $host }. Showing cached mail; { $n } changes go to the server when the connection is back.
}
states-retry = Retry now
states-banner-hide = Hide the network message
states-account-offline = offline
states-account-offline-hint = No connection to the server — showing the cache
states-lost = No connection to { $host } — working offline
states-back = Connection to { $host } restored

notices-title = Notifications
notices-kept = Since Postman started
notices-all = All
notices-errors = Errors
notices-actions = Actions
notices-today = Today
notices-yesterday = Yesterday
notices-done = done
notices-empty = No notifications.
notices-log = Error details are in the Windows Application log (source Postman).
notices-left = { $n } s
notices-more = +{ $n } more
newmail-title = New messages: { $n }
newmail-more = and { $n } more
