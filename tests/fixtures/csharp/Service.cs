using System;
using System.Collections.Generic;

// User record
public record User(int Id, string Name, string Email);

// Repository interface
public interface IRepository
{
    User? FindById(int id);
    void Save(User user);
    int Count();
}

// Logger interface
public interface ILogger
{
    void Log(string message);
}

// UserService class
public class UserService
{
    private readonly IRepository _repo;
    private readonly ILogger _logger;

    public UserService(IRepository repo, ILogger logger)
    {
        _repo = repo;
        _logger = logger;
    }

    public User? FindById(int id)
    {
        _logger.Log($"Finding user {id}");
        return _repo.FindById(id);
    }

    public void Save(User user) => _repo.Save(user);
    public int Count() => _repo.Count();
}

// AbstractController
public abstract class AbstractController
{
    protected string ViewPath { get; }

    protected AbstractController(string viewPath)
    {
        ViewPath = viewPath;
    }

    public string Render(string template) => $"{ViewPath}/{template}";
}

// UserController extends AbstractController
public class UserController : AbstractController
{
    private readonly UserService _userService;

    public UserController(UserService userService)
        : base("users")
    {
        _userService = userService;
    }

    public string Index() => Render("index");
    public string Show(int id)
    {
        var user = _userService.FindById(id);
        return Render("show");
    }
}

// Utils namespace
namespace Utils
{
    public static class StringHelper
    {
        public static string FormatName(string name) => $"User: {name}";
    }
}
