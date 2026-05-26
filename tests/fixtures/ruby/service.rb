# User class
class User
  attr_accessor :id, :name, :email

  def initialize(id, name, email)
    @id = id
    @name = name
    @email = email
  end
end

# Logger module
module Logger
  def log(message)
    puts "[LOG] #{message}"
  end
end

# Repository module (interface)
module Repository
  def find_by_id(id); end
  def save(user); end
  def count; end
end

# UserService class
class UserService
  include Logger

  def initialize(repo, logger = nil)
    @repo = repo
    @logger = logger
  end

  def find_by_id(id)
    @repo.find_by_id(id)
  end

  def save(user)
    @repo.save(user)
  end

  def count
    @repo.count
  end
end

# AbstractController class
class AbstractController
  attr_reader :view_path

  def initialize(view_path)
    @view_path = view_path
  end

  def render(template)
    "#{view_path}/#{template}"
  end
end

# UserController extends AbstractController
class UserController < AbstractController
  def initialize(user_service)
    super("users")
    @user_service = user_service
  end

  def index
    render("index")
  end

  def show(id)
    user = @user_service.find_by_id(id)
    render("show")
  end
end
