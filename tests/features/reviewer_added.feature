Feature: reviewer added

  A user get's a message when they are added as reviewer to a change.

  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And everybody sends the enable command to the bot
      And a Gerrit project named tools

  Scenario: get a message when added by someone else
     Given Alice uploads a new change to the tools project
       And Alice adds Bob as reviewer to Alice's change
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Added as reviewer"

  Scenario: get a message when adding self
     Given Alice uploads a new change to the tools project
       And Bob adds Bob as reviewer to Alice's change
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Added as reviewer"

  Scenario: do not get a message when implicitly adding self by reviewing
     Given Alice uploads a new change to the tools project
       And Bob replies to Alice's change with Code-Review+2 and the comment "Good change"
      When we check for messages by the bot
      Then there is no message for Bob which includes the text "Added as reviewer"

  Scenario: do not get a message when reviewer added messages are disabled
     Given Bob sends the disable notify_reviewer_added command to the bot
       And Alice uploads a new change to the tools project
       And Alice adds Bob as reviewer to Alice's change
      When we check for messages by the bot
      Then there is no message for Bob which includes the text "Added as reviewer"
