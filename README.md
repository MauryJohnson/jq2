# jq2

"Usage: jq2 <json-file>"

jq> :help

Commands
========

:help
    Show this help.

:info
    Show information about the resident dataset.

:last
    Show the previous successful query.

:clear
    Clear the terminal.

:quit
:exit
    End the session.

JQ examples
================

First record:

    .[0]

First CVE:

    .[0].cve_id

Keys from the first record:

    .[0] | keys

All CVE IDs:

    .[].cve_id

Vulnerable function names:

    .[]
    | select(.is_vul == true)
    | .func_name

Count vulnerable records:

    [.[] | select(.is_vul == true)]
    | length

More memory-efficient vulnerable count:

    map(select(.is_vul == true))
    | length

Group records by CVE:

    group_by(.cve_id)
    | map({
        cve: .[0].cve_id,
        count: length
      })


Root-array protection
=====================

If root is an array.

This will be rejected:

    .cve_id

Use:

    .[].cve_id

for every record, or:

    .[0].cve_id

for one record.


Pager
=====

Query results are streamed into `scroller`

[page 1/1 | Enter: close | Backspace: previous | q: quit]
