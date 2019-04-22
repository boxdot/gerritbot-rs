import os
import hashlib
import paramiko
from binascii import hexlify


class Person:
    def __init__(self, name, email):
        self.fullname = name
        self.email = email
        self.username = name.split(None, 1)[0].lower()
        self.http_password = hexlify(os.urandom(16)).decode("ascii")
        self.ssh_key = paramiko.RSAKey.generate(1024)


class Persons:
    def __init__(self):
        self.persons = {}

    def __iter__(self):
        return iter(self.persons.values())

    def get(self, name):
        try:
            return self.persons[name.lower()]
        except LookupError:
            raise ValueError(f"a person named {name} doesn't exist")

    def create(self, name, email):
        person = Person(name, email)

        if person.username in self.persons:
            raise ValueError(f"person {person.username} already exists")

        self.persons[person.username] = person
        return person
