@echo off
rem Fake gh for srv-cli tests (Windows). %1 is the log file the test chose; the rest are
rem the gh arguments. "run view <id> --log" prints the log; anything else echoes its args.
if not "%2"=="run" goto other
if not "%3"=="view" goto other
if not "%5"=="--log" goto other
type "%~1"
exit /b %errorlevel%
:other
echo fake gh: %2 %3 %4 %5 %6 %7 %8 %9
exit /b 0
