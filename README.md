# jq2
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


MegaVul examples
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

MegaVul's root is an array.

This will be rejected:

    .cve_id

Use:

    .[].cve_id

for every record, or:

    .[0].cve_id

for one record.


Pager
=====

Query results are streamed into `less`.

Useful controls:

    Up / Down      Scroll
    Space          Next page
    b              Previous page
    g              Beginning
    G              End
    /text          Search
    n              Next match
    N              Previous match
    q              Quit pager

Pressing q also stops consuming additional query results.

The original JSON file is parsed once at startup.
Each entered query is independently parsed/compiled.
