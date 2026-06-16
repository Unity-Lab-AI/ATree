// User class
class User {
  final int id;
  final String name;
  final String email;

  User(this.id, this.name, this.email);
}

// Repository abstract class
abstract class Repository {
  User? findById(int id);
  void save(User user);
  int count();
}

// Logger mixin
mixin Logger {
  void log(String message) {
    print('[LOG] $message');
  }
}

// UserService class with Logger mixin
class UserService with Logger {
  final Repository repo;

  UserService(this.repo);

  User? findById(int id) {
    log('Finding user $id');
    return repo.findById(id);
  }

  void save(User user) {
    repo.save(user);
  }

  int count() => repo.count();
}

// AbstractController
abstract class AbstractController {
  final String viewPath;

  AbstractController(this.viewPath);

  String render(String template) {
    return '$viewPath/$template';
  }
}

// UserController extends AbstractController
class UserController extends AbstractController {
  final UserService userService;

  UserController(this.userService) : super('users');

  String index() => render('index');
  String show(int id) {
    final user = userService.findById(id);
    return render('show');
  }
}

// Utils class (namespace)
class Utils {
  static String formatName(String name) {
    return 'User: $name';
  }
}
