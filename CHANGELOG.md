0.8.0 (TBD)
===========

TODO.

This release includes a major rewrite of some of the gerritbot
internals. The project was split into multiple crates: besides the
main one for the bot there's now separate crates for the
interaction with Gerrit and Webex Teams. These should be reusable
separately for independent automation projects.

Feature enhancements:

* There is now a new `version` command that reports the bot's version.
