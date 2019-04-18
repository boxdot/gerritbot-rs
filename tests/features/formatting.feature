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
        [{context.last_created_change[subject]}]({context.urls.changes[last]}) ([tools]({context.urls.projects[tools]})) 👍 +2 (Code-Review) from [Alice Smith]({context.urls.users[alice].reviewer})
        """

  Scenario: review with message
     Given Bob uploads a new change to the tools project
       And Alice replies to Bob's change with Code-Review+2 and the comment "Good job!"
      When we check for messages by the bot
      Then there is a message for Bob with the following text:
        """
        [{context.last_created_change[subject]}]({context.urls.changes[last]}) ([tools]({context.urls.projects[tools]})) 👍 +2 (Code-Review) from [Alice Smith]({context.urls.users[alice].reviewer})

        > Good job!
        """

  Scenario: added as reviewer
     Given Bob uploads a new change to the tools project
       And Bob adds Alice as reviewer to Bob's change
      When we check for messages by the bot
      Then there is a message for Alice with the following text:
        """
        [{context.last_created_change[subject]}]({context.urls.changes[last]}) ([tools]({context.urls.projects[tools]})) by [Bob Jones]({context.urls.users[bob]}) 👓 Added as reviewer
        """

  Scenario: inline comments
     Given Bob uploads a new change to the tools project
       And Bob creates the file "README" with the following content in the change:
       """
       Lorem ipsum dolor sit amet, consectetur adipiscing elit. Curabitur
       volutpat ornare convallis. Sed luctus imperdiet nisl, at malesuada risus
       tincidunt vitae. Donec iaculis, lectus ac tempor ullamcorper, lorem odio
       eleifend quam, id ullamcorper metus lacus quis lorem. Aenean fringilla,
       erat vitae rhoncus ultrices, purus est pharetra erat, ac auctor nunc
       sapien eget justo. Nam in iaculis lacus. Cras sit amet nibh libero. Etiam
       non sem quis tortor efficitur ornare sit amet a eros. Aenean commodo
       tempor lectus, id fermentum nisl semper nec. Mauris quis odio sit amet
       nulla volutpat porta et et turpis.
       """
       And Alice replies to Bob's change with Code-Review-2 and the comment "Boo!" and the following inline comments:
       """
       File: README
       Line 0: You shouldn't be adding this file in the first place.
       Line 8: Who even is Mauris?
       """
      When we check for messages by the bot
      Then there is a message for Bob with the following text:
        """
        [{context.last_created_change[subject]}]({context.urls.changes[last]}) ([tools]({context.urls.projects[tools]})) 👎 -2 (Code-Review) from [Alice Smith]({context.urls.users[alice].reviewer})

        > Boo!

        `README`

        > [Line 0]({context.gerrit.http_url}/#/c/{context.last_created_change[_number]}/2/README@0) by [Alice Smith]({context.urls.users[alice].reviewer}): You shouldn't be adding this file in the first place.

        > [Line 8]({context.gerrit.http_url}/#/c/{context.last_created_change[_number]}/2/README@8) by [Alice Smith]({context.urls.users[alice].reviewer}): Who even is Mauris?

        """
