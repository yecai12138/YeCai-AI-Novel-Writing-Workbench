@echo off
rem 见同目录 fake-claude.mjs。把本目录加进 PATH 后，StoryDesk 会把它当成 claude CLI。
node "%~dp0fake-claude.mjs" %*
