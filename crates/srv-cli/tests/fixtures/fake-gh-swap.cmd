@echo off
rem Fake gh for accept_t15b (Windows). %1 is a log file the test chose; the rest are the gh
rem arguments. "alias list" prints "<log>.aliases", then replaces it with "<log>.aliases2"
rem when that exists, so the second listing (in call) differs from the first (in classify).
rem Anything else appends its args to "<log>.ran" and echoes them.
if not "%2"=="alias" goto other
if not "%3"=="list" goto other
if exist "%~1.aliases" type "%~1.aliases"
if exist "%~1.aliases2" move /y "%~1.aliases2" "%~1.aliases" >nul
exit /b 0
:other
echo %2 %3 %4 %5 %6>>"%~1.ran"
echo fake gh: %2 %3 %4 %5 %6 %7 %8 %9
exit /b 0
