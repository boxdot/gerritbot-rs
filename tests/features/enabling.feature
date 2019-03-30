Feature: enabling the bot
  
  A user needs to be enable the bot by sending it the "enable" command.
  
  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And a Gerrit project named tools
  
  Scenario: upload a change (but there are no users)
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2
      When we check for messages by the bot
      Then there are no messages

  Scenario: enable the bot
     Given Bob sends the enable command to the bot
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Happy reviewing!"

  Scenario: upload a change, enable the bot, get a review
     Given Bob uploads a new change to the tools project
       And Bob sends the enable command to the bot
       And Alice replies to Bob's change with Code-Review+2
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Code-Review"
       And this message includes the text "+2"
