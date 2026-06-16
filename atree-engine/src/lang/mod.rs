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
    Dart,
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
    TypeAnnotation, // @type annotation on a variable/parameter/return
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
            "heritage.class" => CaptureTag::Unknown, // class that has the heritage — not a target
            "assignment" => CaptureTag::Assignment,
            "decorator" => CaptureTag::Decorator,
            "http_client" => CaptureTag::HttpClient,
            "call" => CaptureTag::CallWrapper,
            "import" => CaptureTag::ImportWrapper,
            "heritage" => CaptureTag::HeritageWrapper,
            "type.annotation" => CaptureTag::TypeAnnotation,
            _ => CaptureTag::Unknown,
        }
    }
}

impl CaptureTag {
    pub fn is_definition(&self) -> bool {
        matches!(self,
            CaptureTag::DefinitionClass | CaptureTag::DefinitionFunction |
            CaptureTag::DefinitionMethod | CaptureTag::DefinitionInterface |
            CaptureTag::DefinitionEnum | CaptureTag::DefinitionStruct |
            CaptureTag::DefinitionTrait | CaptureTag::DefinitionProperty |
            CaptureTag::DefinitionVariable | CaptureTag::DefinitionConst |
            CaptureTag::DefinitionModule | CaptureTag::DefinitionMacro |
            CaptureTag::DefinitionNamespace | CaptureTag::DefinitionConstructor |
            CaptureTag::DefinitionType | CaptureTag::DefinitionTypedef |
            CaptureTag::DefinitionUnion | CaptureTag::DefinitionTemplate |
            CaptureTag::DefinitionAnnotation | CaptureTag::DefinitionStatic |
            CaptureTag::DefinitionImpl | CaptureTag::DefinitionRecord |
            CaptureTag::DefinitionDelegate)
    }
}

/// Detect visibility modifier by scanning source text at the start of a definition node.
/// For Rust, the function_item node starts at `pub` (if present), so we check the
/// node's own text. For other languages, we check the line prefix before the node.
pub fn detect_visibility(content: &str, node: tree_sitter::Node) -> Option<String> {
    let node_start = node.start_byte();
    let node_end = node.end_byte().min(content.len());

    // The capture node is the @name sub-node (identifier), not the full definition node.
    // We need to look at the text between the start of the line and the name node
    // to find visibility keywords like `pub`, `export`, `public`, etc.
    let name_start = node.start_byte();
    let line_start_byte = name_start
        .checked_sub(
            content[..name_start]
                .chars()
                .rev()
                .take_while(|&c| c != '\n')
                .map(|c| c.len_utf8())
                .sum::<usize>()
        )
        .unwrap_or(0);
    let line_prefix = content.get(line_start_byte..name_start)?;
    let prefix = line_prefix.trim();

    // Split prefix into words and check if any word is a visibility keyword.
    // This handles "pub async fn" (Rust), "export default function" (JS), etc.
    static VISIBILITY_KEYWORDS: &[(&str, &str)] = &[
        ("pub(crate)", "pub"), ("pub(super)", "pub"), ("pub", "pub"),
        ("export", "export"), ("public", "public"),
        ("protected", "protected"), ("private", "private"),
        ("internal", "internal"), ("open", "open"),
    ];
    for word in prefix.split_whitespace() {
        for &(keyword, normalized) in VISIBILITY_KEYWORDS {
            if word == keyword {
                return Some(normalized.to_string());
            }
        }
    }

    None
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
pub mod dart;
pub mod bash;
pub mod json;
pub mod yaml;

use self::typescript::TypeScriptProvider;
use self::typescript::TSXProvider;
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
use self::dart::DartProvider;
use self::bash::BashProvider;
use self::json::JSONProvider;
use self::yaml::YAMLProvider;

pub fn get_provider_for_extension(ext: &str) -> Option<&'static dyn LanguageProvider> {
    static TS: TypeScriptProvider = TypeScriptProvider;
    static TSX: TSXProvider = TSXProvider;
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
    static DART: DartProvider = DartProvider;
    static JSON: JSONProvider = JSONProvider;
    static YAML: YAMLProvider = YAMLProvider;

    if TS.extensions().contains(&ext) { return Some(&TS); }
    if TSX.extensions().contains(&ext) { return Some(&TSX); }
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
    if DART.extensions().contains(&ext) { return Some(&DART); }
    if JSON.extensions().contains(&ext) { return Some(&JSON); }
    if YAML.extensions().contains(&ext) { return Some(&YAML); }
    None
}
