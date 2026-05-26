import java.util.List;
import java.util.ArrayList;

// User entity
public class User {
    private int id;
    private String name;
    private String email;

    public User(int id, String name, String email) {
        this.id = id;
        this.name = name;
        this.email = email;
    }

    public int getId() { return id; }
    public String getName() { return name; }
    public String getEmail() { return email; }
}

// Repository interface
interface Repository {
    User findById(int id);
    void save(User user);
    int count();
}

// Logger interface
interface Logger {
    void log(String message);
}

// UserService provides user operations
class UserService {
    private Repository repo;
    private Logger logger;

    public UserService(Repository repo, Logger logger) {
        this.repo = repo;
        this.logger = logger;
    }

    public User findById(int id) {
        logger.log("Finding user " + id);
        return repo.findById(id);
    }

    public void save(User user) {
        repo.save(user);
    }

    public int count() {
        return repo.count();
    }
}

// AbstractController base class
abstract class AbstractController {
    protected String viewPath;

    public AbstractController(String viewPath) {
        this.viewPath = viewPath;
    }

    public String render(String template) {
        return viewPath + "/" + template;
    }
}

// UserController extends AbstractController
class UserController extends AbstractController {
    private UserService userService;

    public UserController(UserService userService) {
        super("users");
        this.userService = userService;
    }

    public String index() {
        return render("index");
    }

    public String show(int id) {
        User user = userService.findById(id);
        return render("show");
    }
}
