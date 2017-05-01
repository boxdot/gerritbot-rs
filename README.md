# Gerritbot [![Build Status](https://travis-ci.org/boxdot/gerritbot-rs.svg?branch=master)](https://travis-ci.org/boxdot/gerritbot-rs)

A [Cisco Spark](https://www.ciscospark.com) bot which notifies about +1/-1 from [Gerrit](https://www.gerritcodereview.com).

WIP:

- [x] Implement streaming from Gerrit using SSH `gerrit-events` API.
- [x] Implement Spark message receive endpoint.
- [ ] Introduce a simple human-bot communication protocol.
- [ ] Implement send message to Spark API.
- [ ] Connect: Spark <-> Bot <-> Gerrit.
- [ ] Implement serialization of state.

Nice to have features:

- [ ] Query Gerrit for the list of watched reviews.

Nice to have:

- [ ] Use a more sophisticated command line parser.
- [ ] Get rid off `libssh2` dependency.
- [ ] Proper configurable logging with verbosity mode.
- [ ] Tests.
- [ ] Automatic update of webhooks URL in Spark.
