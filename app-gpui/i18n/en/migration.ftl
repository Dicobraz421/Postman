# Moving between accounts (S8-16, mockup S8-D "Move"): question, progress, pause, no space, stop, result, row icons.
migration-ask = { $n ->
    [one] { $count } message moved from “{ $from }”
   *[other] { $count } messages moved from “{ $from }”
}
migration-ask-more = { $n ->
    [one] { $count } message moved from “{ $from }” and { $more } more folders
   *[other] { $count } messages moved from “{ $from }” and { $more } more folders
}
migration-ask-exchange = · Postman labels won’t be kept on Exchange
migration-to-server = Move to server “{ $server }”
migration-keep-local = Keep as local archive
migration-running = Moving to “{ $to }”
migration-running-detail = · { $done } of { $total } · { $bytes } of { $total_bytes }
migration-running-eta = · about { $eta } left
migration-paused = Move paused
migration-pause = Pause
migration-resume = Resume
migration-stop = Stop
migration-of = of { $total }
migration-no-space = { $n ->
    [one] No space on “{ $server }”: { $count } message left · { $bytes }
   *[other] No space on “{ $server }”: { $count } messages left · { $bytes }
}
migration-keep-local-short = Keep locally
migration-stop-ask = Stop the move: { $done } of { $total } done
migration-stop-keep = Keep the rest locally
migration-stop-return = Move back
migration-stop-no = Keep going
migration-status = Moving: { $done } of { $total } · { $bytes }
migration-status-paused = Move paused: { $done } of { $total }
migration-status-no-space = Move stopped: no space
migration-done = { $n ->
    [one] Moved to “{ $server }”: { $count } message
   *[other] Moved to “{ $server }”: { $count } messages
}
migration-done-source = Deleted from “{ $source }” after “{ $server }” confirmed
migration-done-partial = Moved to “{ $server }”: { $done } of { $total }
migration-done-local = { $n ->
    [one] { $count } message is on this computer only
   *[other] { $count } messages are on this computer only
}
migration-done-local-detail = “{ $to }”, deleted from “{ $source }”
migration-returned = { $n ->
    [one] { $count } message moved back to “{ $from }”
   *[other] { $count } messages moved back to “{ $from }”
}
migration-source-kept = { $n ->
    [one] { $n } message wasn’t deleted at the source: the server refused, it is marked at the target
   *[other] { $n } messages weren’t deleted at the source: the server refused, they are marked at the target
}
migration-core-stopped = Moving is unavailable: the storage thread stopped. The reason is in the log.
migration-row-pending = Waiting to move to “{ $server }”
migration-row-paused = Move paused
migration-row-no-space = Waiting for space on “{ $server }”
migration-row-local = On this computer only — not on “{ $server }”
migration-row-too-large = On this computer only: { $size } exceeds the “{ $server }” limit
migration-size-gb = { $n } GB
migration-eta-min = { $n } min
migration-eta-hours = { $h } h { $m } min
