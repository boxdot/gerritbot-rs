Feature: review comments

  The whole point of the bot: get comments when changes get review comments.

  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And everybody sends the enable command to the bot
      And a Gerrit project named tools
  
  Scenario: review without message
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Code-Review"
       And this message includes the text "+2"

  Scenario: review with message
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2 and the comment "Good job!"
      When we check for messages by the bot
      Then there is a message for Bob which includes the text "Code-Review"
       And this message includes the text "+2"
       And this message includes the text "Good job!"
