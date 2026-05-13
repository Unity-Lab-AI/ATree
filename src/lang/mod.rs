use std::fmt::Debug;
use tree_sitter::Language;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LanguageId {
    TypeScript,
    JavaScript,
    Python,
    Go,
    Rust,
    Java,
    C,
    Cpp,
    CSharp,
    PHP,
    Ruby,
    Kotlin,
    Swift,
    Bash,
    JSON,
    YAML,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureTag {
    DefinitionClass,
    DefinitionFunction,
    DefinitionMethod,
    DefinitionInterface,
    DefinitionEnum,
    DefinitionStruct,
    DefinitionTrait,
    DefinitionProperty,
    DefinitionVariable,
    DefinitionConst,
    DefinitionModule,
    DefinitionMacro,
    DefinitionNamespace,
    DefinitionConstructor,
    DefinitionType,
    DefinitionTypedef,
    DefinitionUnion,
    DefinitionTemplate,
    DefinitionAnnotation,
    DefinitionStatic,
    DefinitionImpl,
    DefinitionRecord,
    DefinitionDelegate,
    CallName,
    ImportSource,
    HeritageExtends,
    HeritageImplements,
    HeritageTrait,
    Assignment,
    Decorator,
    HttpClient,
    CallWrapper,    // @call wrapper capture
    ImportWrapper,  // @import wrapper capture
    HeritageWrapper, // @heritage wrapper capture
    Unknown,
}

impl From<&str> for CaptureTag {
    fn from(s: &str) -> Self {
        match s {
            "definition.class" => CaptureTag::DefinitionClass,
            "definition.function" => CaptureTag::DefinitionFunction,
            "definition.method" => CaptureTag::DefinitionMethod,
            "definition.interface" => CaptureTag::DefinitionInterface,
            "definition.enum" => CaptureTag::DefinitionEnum,
            "definition.struct" => CaptureTag::DefinitionStruct,
            "definition.trait" => CaptureTag::DefinitionTrait,
            "definition.property" => CaptureTag::DefinitionProperty,
            "definition.variable" => CaptureTag::DefinitionVariable,
            "definition.const" => CaptureTag::DefinitionConst,
            "definition.module" => CaptureTag::DefinitionModule,
            "definition.macro" => CaptureTag::DefinitionMacro,
            "definition.namespace" => CaptureTag::DefinitionNamespace,
            "definition.constructor" => CaptureTag::DefinitionConstructor,
            "definition.type" => CaptureTag::DefinitionType,
            "definition.typedef" => CaptureTag::DefinitionTypedef,
            "definition.union" => CaptureTag::DefinitionUnion,
            "definition.template" => CaptureTag::DefinitionTemplate,
            "definition.annotation" => CaptureTag::DefinitionAnnotation,
            "definition.static" => CaptureTag::DefinitionStatic,
            "definition.impl" => CaptureTag::DefinitionImpl,
            "definition.record" => CaptureTag::DefinitionRecord,
            "definition.delegate" => CaptureTag::DefinitionDelegate,
            "call.name" => CaptureTag::CallName,
            "import.source" => CaptureTag::ImportSource,
            "heritage.extends" => CaptureTag::HeritageExtends,
            "heritage.implements" => CaptureTag::HeritageImplements,
            "heritage.trait" => CaptureTag::HeritageTrait,
            "heritage.class" => CaptureTag::HeritageExtends, // class that has the heritage
            "assignment" => CaptureTag::Assignment,
            "decorator" => CaptureTag::Decorator,
            "http_client" => CaptureTag::HttpClient,
            "call" => CaptureTag::CallWrapper,
            "import" => CaptureTag::ImportWrapper,
            "heritage" => CaptureTag::HeritageWrapper,
            _ => CaptureTag::Unknown,
        }
    }
}

pub trait LanguageProvider: Debug + Send + Sync {
    fn id(&self) -> LanguageId;
    fn extensions(&self) -> &'static [&'static str];
    fn tree_sitter_language(&self) -> Language;
    fn query(&self) -> &'static str;
}

pub mod typescript;
pub mod javascript;
pub mod python;
pub mod go;
pub mod rust;
pub mod java;
pub mod c;
pub mod cpp;
pub mod csharp;
pub mod php;
pub mod ruby;
pub mod kotlin;
pub mod swift;
pub mod bash;
pub mod json;
pub mod yaml;

use self::typescript::TypeScriptProvider;
use self::javascript::JavaScriptProvider;
use self::python::PythonProvider;
use self::go::GoProvider;
use self::rust::RustProvider;
use self::java::JavaProvider;
use self::c::CProvider;
use self::cpp::CppProvider;
use self::csharp::CSharpProvider;
use self::php::PHPProvider;
use self::ruby::RubyProvider;
use self::kotlin::KotlinProvider;
use self::swift::SwiftProvider;
use self::bash::BashProvider;
use self::json::JSONProvider;
use self::yaml::YAMLProvider;

pub fn get_provider_for_extension(ext: &str) -> Option<&'static dyn LanguageProvider> {
    static TS: TypeScriptProvider = TypeScriptProvider;
    static JS: JavaScriptProvider = JavaScriptProvider;
    static PY: PythonProvider = PythonProvider;
    static GO: GoProvider = GoProvider;
    static RS: RustProvider = RustProvider;
    static JAVA: JavaProvider = JavaProvider;
    static C: CProvider = CProvider;
    static CPP: CppProvider = CppProvider;
    static CS: CSharpProvider = CSharpProvider;
    static PHP: PHPProvider = PHPProvider;
    static RB: RubyProvider = RubyProvider;
    static KT: KotlinProvider = KotlinProvider;
    static SWIFT: SwiftProvider = SwiftProvider;
    static BASH: BashProvider = BashProvider;
    static JSON: JSONProvider = JSONProvider;
    static YAML: YAMLProvider = YAMLProvider;

    if TS.extensions().contains(&ext) { return Some(&TS); }
    if JS.extensions().contains(&ext) { return Some(&JS); }
    if PY.extensions().contains(&ext) { return Some(&PY); }
    if GO.extensions().contains(&ext) { return Some(&GO); }
    if RS.extensions().contains(&ext) { return Some(&RS); }
    if JAVA.extensions().contains(&ext) { return Some(&JAVA); }
    if C.extensions().contains(&ext) { return Some(&C); }
    if CPP.extensions().contains(&ext) { return Some(&CPP); }
    if CS.extensions().contains(&ext) { return Some(&CS); }
    if PHP.extensions().contains(&ext) { return Some(&PHP); }
    if RB.extensions().contains(&ext) { return Some(&RB); }
    if KT.extensions().contains(&ext) { return Some(&KT); }
    if SWIFT.extensions().contains(&ext) { return Some(&SWIFT); }
    if BASH.extensions().contains(&ext) { return Some(&BASH); }
    if JSON.extensions().contains(&ext) { return Some(&JSON); }
    if YAML.extensions().contains(&ext) { return Some(&YAML); }
    None
}
