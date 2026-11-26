@if defined AAC_USAGE_DIR goto configured
@ai-account-center usage --dir "%~dp0." %*
@exit /b %errorlevel%
:configured
@ai-account-center usage %*
