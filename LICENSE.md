License Information
===================

**English** | [Türkçe](LICENSE.tr.md)

`sqliters` Is Public Domain
---------------------------

The `sqliters` source code, command-line interface, and adapted test suite in
this repository are dedicated to the **Public Domain** and may be used for any
purpose:

> The author disclaims copyright to this source code.  In place of
> a legal notice, here is a blessing:
>
>   *   May you do good and not evil.
>   *   May you find forgiveness for yourself and forgive others.
>   *   May you share freely, never taking more than you give.

Fallback Permissive License (`Unlicense OR CC0-1.0 OR 0BSD`)
------------------------------------------------------------

In jurisdictions that do not recognize copyright disclaimers or direct
dedications to the public domain, `sqliters` is alternatively licensed, at
your option, under any of the following zero-attribution permissive
open-source licenses:

  *  [The Unlicense](https://unlicense.org/)
  *  [Creative Commons Zero v1.0 Universal (CC0-1.0)](https://creativecommons.org/publicdomain/zero/1.0/legalcode)
  *  [BSD Zero Clause License (0BSD)](https://opensource.org/license/0bsd)

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted.

Experimental Software & Disclaimer of Warranty
----------------------------------------------

THIS SOFTWARE IS EXPERIMENTAL AND IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE, DATA DURABILITY, OR
NONINFRINGEMENT.  IT IS SUITABLE FOR EXPERIMENTAL USE ONLY; NO GUARANTEE IS
GIVEN FOR SERIOUS, CRITICAL, OR PRODUCTION PROJECTS.  IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES, DATA LOSS, OR
OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT, OR OTHERWISE, ARISING
FROM, OUT OF, OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
IN THE SOFTWARE.

Independent Project & Upstream Reference Notice
-----------------------------------------------

`sqliters` is an independent, experimental Rust port based on the public
architecture, file format specification, and public-domain test suite of
[SQLite](https://sqlite.org/) (version 3.54.1).  `sqliters` is **not**
affiliated with, endorsed by, or maintained by the official SQLite development
team or Hipp, Wyrick & Company, Inc.

The upstream SQLite database engine, extensions, and test suite designed by
D. Richard Hipp and the SQLite authors are in the
[Public Domain](https://sqlite.org/copyright.html).  Only clean-room Rust
implementations and adaptations of SQLite's public-domain `.test` scenarios
are distributed in this repository; none of SQLite's third-party C build
configuration scripts (`configure`, `autosetup/`, `autoconf/`) are included.
