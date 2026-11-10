# Search in the command bar (S5-10): results header, "Refine" chips, matches in the message.
search-title = Search: { $query }
search-summary = { $n ->
    [one] { $n } result
   *[other] { $n } results
} · { $place }
search-summary-waiting = { $n ->
    [one] { $n } result
   *[other] { $n } results
} · { $place } · searching the server…
search-where-all = all folders
search-close = Close search — back to the folder
search-refine = Refine
search-refine-from = from: { $name }
search-refine-to-me = to: me
search-refine-attachment = has: attachments
search-refine-unread = unread
search-refine-within = last { $n ->
    [one] { $n } day
   *[other] { $n } days
}
search-refine-category = category: { $name }
search-empty = Nothing found. Remove refinements or search everywhere.
search-matches = Match { $k } of { $n }
search-matches-none = Matches: none in the text
search-matches-query = “{ $query }”
search-refine-only = only { $folder }
search-clear-refines = Clear refinements
