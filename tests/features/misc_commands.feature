Feature:  miscellaneous commands

  Background:
    Given a person named Alice Smith with email address alice@bloom.com

  Scenario: help command
     Given Alice sends the help command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "Commands:"
       And this message includes the text "`enable`"
       And this message includes the text "`disable`"
       And this message includes the text "`status`"
       And this message includes the text "`help`"

  Scenario: status command when disabled
     Given Alice sends the status command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the following text:
        """
        Notifications for you are **disabled**
        """

  Scenario: status command when disabled
     Given Alice sends the enable command to the bot
       And Alice sends the status command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the following text:
        """
        Notifications for you are **enabled**
        """

  Scenario: status with no other users
     Given Alice sends the enable command to the bot
       And Alice sends the status command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "notifying no other users."

  Scenario: status with one other user
     Given a person named Bob Jones with email address bob@jones.com
       And Bob sends the enable command to the bot
       And Alice sends the enable command to the bot
       And Alice sends the status command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "notifying another user."

  Scenario: status with a few other users
     Given the following persons:
       | name         | email                 |
       | Bob Jones    | bob@jones.com         |
       | Eve Harris   | eve@jones.com         |
       | Gene Generic | info@generic-gene.com |
       | Shawn Pearce | sop@google.com        |
       And everybody sends the enable command to the bot
       But Shawn sends the disable command to the bot
      When Alice sends the status command to the bot
       And we check for messages by the bot
      Then there is a message for Alice which includes the text "notifying another 3 users."

  Scenario: version command
     Given Alice sends the version command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the text "gerritbot"

  Scenario: unknown command
     Given Alice sends the bla bla bla command to the bot
      When we check for messages by the bot
      Then there is a message for Alice which includes the following text:
        """
        Hi. I am GerritBot.
        """
