// @generated automatically by Diesel CLI.

diesel::table! {
    annotations (id) {
        id -> Nullable<BigInt>,
        uuid -> Text,
        kind -> Integer,
        start_utc_ms -> BigInt,
        end_utc_ms -> BigInt,
        category_id -> Nullable<BigInt>,
        label -> Nullable<Text>,
        note -> Nullable<Text>,
        created_utc_ms -> BigInt,
        updated_utc_ms -> BigInt,
    }
}

diesel::table! {
    applications (id) {
        id -> Nullable<BigInt>,
        identity_key -> Text,
        kind -> Integer,
        exe_path -> Nullable<Text>,
        aumid -> Nullable<Text>,
        exe_name -> Nullable<Text>,
        display_name -> Text,
        first_seen_utc_ms -> BigInt,
    }
}

diesel::table! {
    availability_intervals (id) {
        id -> Nullable<BigInt>,
        run_id -> BigInt,
        clock_segment_id -> BigInt,
        reason -> Integer,
        start_utc_ms -> BigInt,
        start_mono_ms -> BigInt,
        end_utc_ms -> Nullable<BigInt>,
        end_mono_ms -> Nullable<BigInt>,
        confirmed_utc_ms -> Nullable<BigInt>,
        confirmed_mono_ms -> Nullable<BigInt>,
        end_reason -> Nullable<Integer>,
        last_input_utc_ms -> Nullable<BigInt>,
        detected_utc_ms -> Nullable<BigInt>,
        threshold_ms -> Nullable<BigInt>,
        exception -> Nullable<Integer>,
        policy_version -> BigInt,
    }
}

diesel::table! {
    browser_streams (id) {
        id -> Nullable<BigInt>,
        browser -> Text,
        profile_instance_id -> Text,
        extension_id -> Text,
        label -> Nullable<Text>,
        first_seen_utc_ms -> BigInt,
        last_seen_utc_ms -> BigInt,
    }
}

diesel::table! {
    capture_gaps (id) {
        id -> Nullable<BigInt>,
        run_id -> BigInt,
        clock_segment_id -> BigInt,
        start_utc_ms -> BigInt,
        end_utc_ms -> BigInt,
        start_mono_ms -> BigInt,
        end_mono_ms -> BigInt,
        reason -> Integer,
    }
}

diesel::table! {
    capture_runs (id) {
        id -> Nullable<BigInt>,
        device_id -> Text,
        win_session_id -> BigInt,
        app_version -> Text,
        started_utc_ms -> BigInt,
        confirmed_utc_ms -> Nullable<BigInt>,
        ended_utc_ms -> Nullable<BigInt>,
        end_reason -> Nullable<Integer>,
    }
}

diesel::table! {
    categories (id) {
        id -> Nullable<BigInt>,
        uuid -> Text,
        parent_id -> Nullable<BigInt>,
        builtin_key -> Nullable<Text>,
        name -> Nullable<Text>,
        color -> Text,
        tendency -> Integer,
        sort_order -> BigInt,
        archived -> Integer,
    }
}

diesel::table! {
    clock_segments (id) {
        id -> Nullable<BigInt>,
        run_id -> BigInt,
        start_utc_ms -> BigInt,
        start_mono_ms -> BigInt,
        confirmed_utc_ms -> Nullable<BigInt>,
        confirmed_mono_ms -> Nullable<BigInt>,
        end_utc_ms -> Nullable<BigInt>,
        end_mono_ms -> Nullable<BigInt>,
        end_reason -> Nullable<Integer>,
    }
}

diesel::table! {
    data_day_versions (source, utc_date) {
        source -> Integer,
        utc_date -> BigInt,
        version -> BigInt,
    }
}

diesel::table! {
    deletion_barriers (id) {
        id -> Nullable<BigInt>,
        start_utc_ms -> BigInt,
        end_utc_ms -> BigInt,
        scope -> Text,
        created_utc_ms -> BigInt,
    }
}

diesel::table! {
    domains (id) {
        id -> Nullable<BigInt>,
        host -> Text,
    }
}

diesel::table! {
    enum_labels (enum_name, code) {
        enum_name -> Text,
        code -> Integer,
        label -> Text,
        deprecated -> Integer,
    }
}

diesel::table! {
    ingest_receipts (stream_id, epoch, message_seq) {
        stream_id -> BigInt,
        epoch -> BigInt,
        message_seq -> BigInt,
        received_utc_ms -> BigInt,
        outcome -> Integer,
    }
}

diesel::table! {
    intervals (id) {
        id -> Nullable<BigInt>,
        run_id -> BigInt,
        sequence -> BigInt,
        clock_segment_id -> BigInt,
        kind -> Integer,
        no_foreground_reason -> Nullable<Integer>,
        app_id -> Nullable<BigInt>,
        window_instance_id -> Nullable<BigInt>,
        title_id -> Nullable<BigInt>,
        title_status -> Integer,
        capture_level -> Integer,
        privacy_rule_id -> Nullable<Text>,
        privacy_label_key -> Nullable<Text>,
        privacy_label -> Nullable<Text>,
        session_key -> BigInt,
        start_utc_ms -> BigInt,
        start_mono_ms -> BigInt,
        end_utc_ms -> Nullable<BigInt>,
        end_mono_ms -> Nullable<BigInt>,
        confirmed_utc_ms -> Nullable<BigInt>,
        confirmed_mono_ms -> Nullable<BigInt>,
        duration_ms -> Nullable<BigInt>,
        end_reason -> Nullable<Integer>,
    }
}

diesel::table! {
    rule_set_versions (set_id, version) {
        set_id -> BigInt,
        version -> BigInt,
        content -> Text,
        content_hash -> Text,
        parent_version -> Nullable<BigInt>,
        note -> Nullable<Text>,
        created_utc_ms -> BigInt,
    }
}

diesel::table! {
    rule_sets (id) {
        id -> Nullable<BigInt>,
        key -> Text,
        kind -> Integer,
        parent_id -> Nullable<BigInt>,
        source -> Integer,
        name -> Text,
        enabled -> Integer,
        sibling_order -> BigInt,
        current_version -> BigInt,
        chain_id -> Nullable<BigInt>,
        created_utc_ms -> BigInt,
    }
}

diesel::table! {
    settings (key) {
        key -> Text,
        value -> Text,
    }
}

diesel::table! {
    titles (id) {
        id -> Nullable<BigInt>,
        text -> Text,
    }
}

diesel::table! {
    web_intervals (id) {
        id -> Nullable<BigInt>,
        stream_id -> BigInt,
        interval_id -> BigInt,
        start_utc_ms -> BigInt,
        end_utc_ms -> Nullable<BigInt>,
        confirmed_utc_ms -> Nullable<BigInt>,
        end_reason -> Nullable<Integer>,
        domain_id -> Nullable<BigInt>,
        tab_title_id -> Nullable<BigInt>,
        tab_title_status -> Integer,
        tab_id -> Nullable<BigInt>,
        browser_window_id -> Nullable<BigInt>,
        navigation_id -> Nullable<BigInt>,
        audible -> Integer,
        link_status -> Integer,
        web_level -> Integer,
        privacy_rule_id -> Nullable<Text>,
        privacy_label_key -> Nullable<Text>,
        privacy_label -> Nullable<Text>,
    }
}

diesel::table! {
    window_instances (id) {
        id -> Nullable<BigInt>,
        run_id -> BigInt,
        app_id -> BigInt,
        pid -> BigInt,
        process_created_utc_ms -> Nullable<BigInt>,
        hwnd -> BigInt,
        generation -> BigInt,
        retired_utc_ms -> Nullable<BigInt>,
        window_class -> Nullable<Text>,
        first_seen_utc_ms -> BigInt,
        last_seen_utc_ms -> BigInt,
    }
}

diesel::joinable!(annotations -> categories (category_id));
diesel::joinable!(availability_intervals -> capture_runs (run_id));
diesel::joinable!(availability_intervals -> clock_segments (clock_segment_id));
diesel::joinable!(capture_gaps -> capture_runs (run_id));
diesel::joinable!(capture_gaps -> clock_segments (clock_segment_id));
diesel::joinable!(clock_segments -> capture_runs (run_id));
diesel::joinable!(ingest_receipts -> browser_streams (stream_id));
diesel::joinable!(intervals -> applications (app_id));
diesel::joinable!(intervals -> capture_runs (run_id));
diesel::joinable!(intervals -> clock_segments (clock_segment_id));
diesel::joinable!(intervals -> titles (title_id));
diesel::joinable!(intervals -> window_instances (window_instance_id));
diesel::joinable!(rule_set_versions -> rule_sets (set_id));
diesel::joinable!(web_intervals -> browser_streams (stream_id));
diesel::joinable!(web_intervals -> domains (domain_id));
diesel::joinable!(web_intervals -> intervals (interval_id));
diesel::joinable!(web_intervals -> titles (tab_title_id));
diesel::joinable!(window_instances -> applications (app_id));
diesel::joinable!(window_instances -> capture_runs (run_id));

diesel::allow_tables_to_appear_in_same_query!(
    annotations,
    applications,
    availability_intervals,
    browser_streams,
    capture_gaps,
    capture_runs,
    categories,
    clock_segments,
    data_day_versions,
    deletion_barriers,
    domains,
    enum_labels,
    ingest_receipts,
    intervals,
    rule_set_versions,
    rule_sets,
    settings,
    titles,
    web_intervals,
    window_instances,
);
