# "Export…" (S8-32, mockup S8-D "Export"): choice bar, progress, result.
export-menu = Export…
export-what-folder = { $n ->
    [one] Export “{ $folder }” · { $count } message
   *[other] Export “{ $folder }” · { $count } messages
}
export-what-selection = { $n ->
    [one] Export: { $count } message
   *[other] Export: { $count } messages
}
export-what-account = { $n ->
    [one] Export account “{ $account }” · { $count } message
   *[other] Export account “{ $account }” · { $count } messages
}
export-what-all = { $n ->
    [one] Export everything · { $count } message
   *[other] Export everything · { $count } messages
}
export-subfolders = Include subfolders ({ $n })
export-start = Choose folder and start…
export-cancel = Cancel
export-eml = One file per message · Outlook, Thunderbird, Apple Mail
export-mbox = One file per folder · Thunderbird, Apple Mail
export-maildir = One directory per folder · mutt, Dovecot
export-msg = One file per message · Outlook: drag into a folder
export-pst = Outlook data file · “Import and Export”
export-running = Exporting to { $format }
export-fetching = downloading from server: { $n }
export-status = Export: { $done } of { $total }
export-stop = Stop
export-done = { $n ->
    [one] Exported { $count } message
   *[other] Exported { $count } messages
}
export-done-partial = Exported { $done } of { $total }
export-failed-fetch = { $n ->
    [one] { $count } message couldn’t be downloaded from “{ $server }”
   *[other] { $count } messages couldn’t be downloaded from “{ $server }”
}
export-stopped = Export stopped: { $done } of { $total } written
export-stopped-detail = Finished files are intact, the unfinished one was removed
export-open = Open folder
