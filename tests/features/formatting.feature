Feature: message formatting

  Check exact formatting of messages.

  Background:
    Given a person named Alice Smith with email address alice@bloom.com
      And a person named Bob Jones with email address bob@jones.com
      And everybody sends the enable command to the bot
      And a Gerrit project named tools

  Scenario: review without message
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2
      When we check for messages by the bot
      Then there is a message for Bob with the following text:
        """
        [{context.last_created_change[subject]}]({context.gerrit.http_url}/{context.last_created_change[_number]}) (tools) 👍 +2 (Code-Review) from alice
        """

  Scenario: review with message
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2 and the comment "Good job!"
      When we check for messages by the bot
      Then there is a message for Bob with the following text:
        """
        [{context.last_created_change[subject]}]({context.gerrit.http_url}/{context.last_created_change[_number]}) (tools) 👍 +2 (Code-Review) from alice

        > Good job!
        """
