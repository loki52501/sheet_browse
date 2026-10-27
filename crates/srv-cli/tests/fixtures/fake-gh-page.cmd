@echo off
rem Fake gh for accept_t15b (Windows). %1 is a page file the test wrote. "alias list" prints
rem nothing; any other call prints the page to stdout, or to stderr and exits 1 when
rem "<page>.fail" exists. When "<page>.aliasfail" exists, "alias list" fails (exit 1).
if not "%2"=="alias" goto notalias
if not exist "%~1.aliasfail" exit /b 0
echo alias list failed 1>&2
exit /b 1
:notalias
if exist "%~1.fail" goto fail
type "%~1"
exit /b 0
:fail
type "%~1" 1>&2
exit /b 1
