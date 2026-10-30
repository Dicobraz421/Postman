# "On this computer" and moving between accounts (S8-15, mockup S8-D "Move").
local-group = On this computer
local-group-hint = { $n ->
    [one] Stored only in Postman’s database on this computer · { $count } message
   *[other] Stored only in Postman’s database on this computer · { $count } messages
}
local-new-folder = New folder
local-rename = Rename
local-delete = Delete
local-delete-not-empty = Only an empty folder can be deleted
local-drop = { $n ->
    [one] Move { $n } message here
   *[other] Move { $n } messages here
}
