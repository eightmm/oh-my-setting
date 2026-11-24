---
name: oms-connect
description: Connect this desktop plugin to an OMS project during setup.
---

Ask which current project the user wants to manage if the chat has no clear
project. Call `oms_list_projects` and verify an existing selection with
`oms_panel_snapshot`. When the host already exposes the correct project, show
how to give its Main a goal in this conversation.

When `oms_list_projects` succeeds with `binding: "unbound"`, a direct chat
with local shell access can register its current trusted checkout. Resolve its
physical Git root or adopted OMS project root. Show the proposed alias and
checkout, explain that every user of this plugin connection could manage that project, and obtain
the user's permission to persist the mapping. Use the OMS account's config
file `oh-my-setting/desktop-projects.json` under its absolute `XDG_CONFIG_HOME`
or `~/.config`. Create the missing map exclusively as a private,
owner-controlled JSON object. If it already exists or appears during setup,
refresh the project list and leave the existing configuration to its owner.
Respect configured host pins and explicit catalog overrides. Verify the alias
in a fresh `oms_list_projects` call or after the host reconnects.
