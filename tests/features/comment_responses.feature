Feature: responses to review comments

  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And everybody sends the enable command to the bot
      And everybody sends the enable notify_review_responses command to the bot
      And a Gerrit project named tools
  
  Scenario: comment response
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review-2 and the comment "I don't like it."
       And Bob replies to the change with the comment "Please reconsider."
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "Please reconsider"
