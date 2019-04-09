BDD
---

To run the BDD test suite you need to install `behave` and a few other Python
packages first. The easiest way to do that is to use pip:

```
$ pip3 install -r requirements.txt
```
    
Afterwards you should be able to just run behave:

```
$ behave
```

If for some reason the `behave` binary wasn't put into your `PATH` you can run
the installed package directly:

```
$ python3 -mbehave
```

Please check the [behave documentation](https://behave.readthedocs.io/en/latest/behave.html)
for the available command line options.
