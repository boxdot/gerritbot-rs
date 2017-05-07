# Gerritbot [![Build Status](https://travis-ci.org/boxdot/gerritbot-rs.svg?branch=master)](https://travis-ci.org/boxdot/gerritbot-rs)

A [Cisco Spark](https://www.ciscospark.com) bot which notifies about +1/-1 from [Gerrit](https://www.gerritcodereview.com).

WIP:

- [x] Implement streaming from Gerrit using SSH `gerrit-events` API.
- [x] Implement Spark message receive endpoint.
- [x] Introduce a simple human-bot communication protocol.
- [x] Implement send message to Spark API.
- [x] Implement Gerrit account verification.
- [x] Connect: Spark <-> Bot <-> Gerrit.
- [x] Implement serialization of state.
- [ ] Remove the task list and write a proper README.

Nice to have:

- [ ] Automatic update of webhooks URL in Spark.
- [ ] Use a more sophisticated command line parser.
- [ ] Get rid off `libssh2` dependency.
- [ ] Proper configurable logging with verbosity mode.
- [ ] Tests.
