Feature: notifications when a change is closed

  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And a person named Eve Harris with email address eve@jones.com
      And everybody sends the enable command to the bot
      And everybody sends the enable notify_change_merged command to the bot
      And everybody sends the enable notify_change_abandoned command to the bot
      And a Gerrit project named tools
  
  Scenario: change abandoned by owner
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review-2 and the comment "I don't like it."
       And Bob abandons the change
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "Abandoned"
      Then there is no message for Bob which includes the text "Abandoned"

  Scenario: change abandoned by other
     Given Bob uploads a new change to the tools project
       And Alice abandons Bob's change
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Abandoned"
      Then there is no message for Alice which includes the text "Abandoned"

  Scenario: change submitted by owner
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2 and the comment "I like it."
       And Bob submits the change
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "Submitted"
      Then there is no message for Bob which includes the text "Submitted"

  Scenario: change submitted by other
     Given Bob uploads a new change to the tools project
       And Eve replies to Bob's change with Code-Review+2 and the comment "Whatever."
       And Alice submits Bob's change
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Submitted"
      Then there is a message for Eve which includes the text "Submitted"
      Then there is no message for Alice which includes the text "Submitted"