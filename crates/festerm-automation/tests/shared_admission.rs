use festerm_automation::{
    automation_admission_lock, try_automation_admission_lock, AutomationAdmissionGuard,
    AutomationAdmissionTryError,
};
use festerm_session::AutomationEpoch;
use std::time::{Duration, Instant};

#[test]
fn session_and_kernel_share_admission_and_poison() {
    const CHILD: &str = "FESTERM_SHARED_ADMISSION_POISON_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let epoch = AutomationEpoch::new();
        epoch.transition(true);
        assert!(epoch.capture().is_some());
        assert!(std::thread::spawn(|| {
            let _guard = automation_admission_lock().unwrap();
            panic!("poison the shared admission lock in this isolated process");
        })
        .join()
        .is_err());
        assert!(automation_admission_lock().is_none());
        assert!(festerm_session::automation_admission_lock().is_none());
        assert!(matches!(
            try_automation_admission_lock(),
            Err(AutomationAdmissionTryError::Poisoned)
        ));
        assert!(matches!(
            festerm_session::try_automation_admission_lock(),
            Err(AutomationAdmissionTryError::Poisoned)
        ));
        assert_eq!(epoch.capture(), None);
        epoch.transition(true);
        assert_eq!(epoch.capture(), None);
        let fresh = AutomationEpoch::new();
        fresh.transition(true);
        assert_eq!(fresh.capture(), None);
        return;
    }

    let session_guard: AutomationAdmissionGuard =
        festerm_session::automation_admission_lock().unwrap();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        sender
            .send(matches!(
                try_automation_admission_lock(),
                Err(AutomationAdmissionTryError::Busy)
            ))
            .unwrap();
    });
    assert!(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
    worker.join().unwrap();
    drop(session_guard);

    let kernel_guard = automation_admission_lock().unwrap();
    assert!(matches!(
        festerm_session::try_automation_admission_lock(),
        Err(AutomationAdmissionTryError::Busy)
    ));
    let epoch = AutomationEpoch::new();
    epoch.transition_under_admission(&kernel_guard, true);
    let generation = epoch.capture_under_admission(&kernel_guard).unwrap();
    assert!(epoch.is_current_under_admission(&kernel_guard, generation));
    drop(kernel_guard);

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "session_and_kernel_share_admission_and_poison",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "isolated poison test failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated poison test timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
