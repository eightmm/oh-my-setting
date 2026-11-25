---
name: oms-connect
description: Connect this desktop plugin to an OMS project during setup.
---

Call `oms_list_projects` and offer the project aliases returned by the
connected OMS host. Verify the selected alias with `oms_panel_snapshot`.
When that succeeds, show how to give its Main a goal in this conversation.

If the connection is unavailable, explain that the desktop's OpenSSH alias
`oms-runtime` must reach a trusted host noninteractively, with the host key
already verified and an authorized key available. The remote account needs
the OMS build with desktop tools and a private `oh-my-setting/desktop-projects.json`
alias map under its absolute `XDG_CONFIG_HOME`, or under `~/.config` when unset or empty. Show which
prerequisite failed when the host provides evidence. Keep the SSH identity,
host registration and authorized project list under their owner's control.
