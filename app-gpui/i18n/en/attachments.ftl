# Attachments: ribbon under the header, built-in preview, macros (S5-07).
att-ribbon = Attachments · { $n } · { $size }
att-save-all = Save all…
att-more = +{ $n }
att-less = Collapse
att-open = Open
att-save = Save as…
att-preview = Preview
att-loading = downloading…
att-status-macros = Contains macros · opening blocked
att-status-blocked = Executable · save only
att-status-archive = Archive
att-saved-all = { $n ->
    [one] Saved { $n } file: { $dir }
   *[other] Saved { $n } files: { $dir }
}
att-save-all-failed = “{ $name }” is not saved: { $error }

att-pv-close = Close preview
att-pv-note-builtin = Built-in preview · no external programs started
att-pv-note-archive = Archive contents · nothing is extracted to disk
att-pv-note-check = Attachment check · the file was not opened
att-pv-loading = The file is being parsed in the sandbox…
att-pv-failed = Preview failed: { $error }
att-pv-too-big = The file is larger than { $size } — no built-in preview, the file can be saved.
att-pv-page = { $page } / { $pages }
att-pv-prev-page = Previous page
att-pv-next-page = Next page
att-pv-image-info = { $w } × { $h } px · { $format }
att-pv-open-app = Open in app
att-pv-save = Save…
att-pv-col-name = Name
att-pv-col-size = Size
att-pv-col-date = Modified
att-pv-locked = encrypted
att-pv-archive-total = { $n ->
    [one] { $n } file · { $size } unpacked
   *[other] { $n } files · { $size } unpacked
}
att-pv-archive-more = { $n } more in the archive
att-pv-archive-safe = No executable files
att-pv-archive-danger = { $n ->
    [one] { $n } dangerous file: executable or with macros
   *[other] { $n } dangerous files: executable or with macros
}
att-pv-archive-encrypted = Some files are encrypted — their contents cannot be checked
att-pv-archive-bomb = Suspiciously high compression (possible zip bomb) — do not extract
att-pv-text-cut = Showing the beginning of the file — { $size }
att-pv-doc-cut = Showing the beginning of the document
att-pv-none = There is no built-in preview for this file type.
att-pv-office-clean = No macros found. The built-in preview does not show document contents.
att-pv-blocked = Opening blocked
att-pv-macros = Contains macros
att-pv-exe = Executable file or script
att-pv-found = Found
att-pv-found-macros = { $n ->
    [one] VBA macros: { $n } module
   *[other] VBA macros: { $n } modules
}
att-pv-found-project = VBA project (modules not listed)
att-pv-found-type = Macro-enabled file type ({ $ext })
att-pv-found-exe = The file runs as a program ({ $ext })
att-pv-why = Why blocked
att-pv-why-macros = Macros run code on the computer. Such files are not opened from mail.
att-pv-why-exe = Programs and scripts from mail are not started.
att-pv-save-warning = The file is saved marked “from the Internet”: Office opens it in Protected View with macros disabled.
att-pv-save-off = Saving files with macros is turned off (Settings → Security).

# Dragging an attachment to another app (T1-18).
att-drag-ready = “{ $name }” is downloaded — drag it again
att-drag-failed = Could not drag “{ $name }”: { $error }
# Preview sections (S8-23) and viewer modules (S8-27), mockup S8-D "Preview".
att-pv-sheet = Sheet “{ $name }”
att-pv-slide = Slide { $n }
att-pv-figure = [picture]
att-pv-notes = Speaker notes
att-pv-cut-rows-shown = Showing the first { $shown } rows
att-pv-prev-slide = Previous slide (←)
att-pv-next-slide = Next slide (→)
att-pv-letter-from = From
att-pv-letter-to = To
att-pv-letter-cc = Cc
att-pv-letter-date = Date
att-pv-module = Viewer: module “{ $name }” { $version }
att-pv-module-third = Viewer: module “{ $name }” { $version } (third-party) · sandboxed
att-pv-module-enable = Turn on module “{ $name }”
att-pv-module-refused = The module couldn’t show the file: { $reason }
