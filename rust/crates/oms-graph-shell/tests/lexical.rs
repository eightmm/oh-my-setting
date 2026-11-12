use oms_graph_shell::{command_names, shell_code};

#[test]
fn commands_preserve_order_and_wrapper_handling() {
    assert_eq!(
        command_names("if A=x sudo -n B=y helper; echo $( ! nested x)"),
        ["nested", "helper", "echo"]
    );
    assert!(command_names("").is_empty());
    assert!(command_names("A=x; env -n").is_empty());
    assert_eq!(command_names("bad-name; good"), ["good"]);
}

#[test]
fn case_control_ignores_quotes_and_comments() {
    assert_eq!(shell_code("case x in # esac"), ("case x in ".into(), false));
    assert_eq!(shell_code("printf 'case esac'"), ("printf   ".into(), true));
    assert_eq!(shell_code(""), (String::new(), false));
    assert_eq!(shell_code("echo \\#tag"), ("echo   tag".into(), false));
    assert_eq!(shell_code("echo 'unterminated ;;"), ("echo  ".into(), true));
}

#[test]
fn case_depth_recognizes_command_positions() {
    use oms_graph_shell::{case_close_count, case_open_count};
    assert_eq!(case_open_count("x) case y in"), 1);
    assert_eq!(case_open_count("case x in; case y in"), 2);
    assert_eq!(case_open_count("echo case x in"), 0);
    assert_eq!(case_open_count("case x"), 0);
    assert_eq!(case_open_count(""), 0);
    assert_eq!(case_close_count("esac ;; esac"), 2);
    assert_eq!(case_close_count("echo esac"), 0);
    assert_eq!(case_close_count(""), 0);
}
