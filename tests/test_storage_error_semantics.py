from echo_agent.storage.errors import StorageError, StorageUnavailable, CorruptData


def test_exception_hierarchy():
    assert issubclass(StorageUnavailable, StorageError)
    assert issubclass(CorruptData, StorageError)
    assert issubclass(StorageError, Exception)


def test_exceptions_carry_message():
    err = StorageUnavailable("db is gone")
    assert str(err) == "db is gone"
    assert isinstance(err, StorageError)
