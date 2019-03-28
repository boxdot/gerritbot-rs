from hamcrest import *


@when(u"we check for messages by the bot")
def step_impl(context):
    context.bot.current_messages = context.bot.messages
    del context.bot.messages[:]


@then(u"there are no messages")
def step_impl(context):
    assert_that(context.bot.current_messages, empty())


@then(u'there is a message for {person} which includes the text "{text}"')
def step_impl(context, person, text):
    assert_that(
        context.bot.current_messages,
        contains(has_property("message", contains_string(text))),
    )


@given(u"{sender} sends the {command} command to the bot")
def step_impl(context, sender, command):
    sender = context.persons.get(sender)
    context.bot.send_message(sender, command)
