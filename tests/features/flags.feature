Feature: enabling or disabling features of the bot

  A user needs to be enable the bot by sending it the "enable" command.

  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And Bob sends the enable command to the bot
      And a Gerrit project named tools

  Scenario: enable a feature
     Given Bob sends the enable notify_review_approvals command to the bot
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Flag notify_review_approvals **enabled**"

  Scenario: upload a change, enable the bot, disable reviews, don't get a review
     Given Bob uploads a new change to the tools project
       And Bob sends the disable notify_review_approvals command to the bot
       And Alice replies to Bob's change with Code-Review+2
      When we check for messages by the bot
      Then there is no message for Bob which includes the text "Code-Review"

  Scenario: enabled flag shows up in status
     Given Bob sends the enable notify_change_abandoned command to the bot
       And Bob sends the status command to the bot
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Notifications for you are **enabled**"
       And this message includes the text "notify_change_abandoned"

  Scenario: disabled flag does not show up in status
     Given Bob sends the disable notify_change_abandoned command to the bot
       And Bob sends the status command to the bot
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Notifications for you are **enabled**"
       And this message does not include the text "notify_change_abandoned"