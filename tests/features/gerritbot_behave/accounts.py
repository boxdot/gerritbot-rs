import os
import hashlib
import paramiko
from binascii import hexlify


class Account:
    def __init__(self, *, fullname, username):
        self.fullname = fullname
        self.username = username
        self.http_password = hexlify(os.urandom(16)).decode("ascii")
        self.ssh_key = paramiko.RSAKey.generate(1024)


class Person(Account):
    def __init__(self, name, email):
        super().__init__(fullname=name, username=name.split(None, 1)[0].lower())
        self.email = email


class Bot(Account):
    def __init__(self, name):
        super().__init__(fullname=name, username=name.lower())


class Accounts:
    def __init__(self):
        self.accounts = {}

    def all_persons(self):
        return [
            account for account in self.accounts.values() if isinstance(account, Person)
        ]

    def get_account(self, name, *, expected_type=None):
        try:
            account = self.accounts[name.lower()]
        except LookupError:
            raise ValueError(f"account named {name} doesn't exist")

        if expected_type is None or isinstance(account, expected_type):
            return account
        else:
            raise ValueError(
                f"account named {name} is not an instance of {expected_type.__name__}"
            )

    def get_person(self, name):
        return self.get_account(name, expected_type=Person)

    def add_account(self, account):
        if account.username in self.accounts:
            raise ValueError(f"account {account.username} already exists")

        self.accounts[account.username] = account

    def create_bot(self, name):
        account = Bot(name)
        self.add_account(account)
        return account

    def create_person(self, name, email):
        account = Person(name, email)
        self.add_account(account)
        return account
